//! App state and the backend→UI message taxonomy.
//!
//! The message shape mirrors `fauna-desktop` (`apps/linux.md` § Message
//! Flow): background tokio tasks send `UiMessage` values over an mpsc channel;
//! the render loop applies them to [`App`].
//!
//! **The enum does not lift to shared Rust** — assessed against linux
//! 2026-07-16 and recorded in `apps/tui.md` § Architecture (Event-loop
//! bullet). Both apps' payloads are deliberately client-shaped (linux
//! carries GTK/db row types, tui carries page [`PageOutcome`]s), so only the
//! outer Data/Realtime taxonomy is common, and that is a **convention, not
//! liftable code**. The real priority-#2 surface between the two Rust-native
//! apps is the shared managers, projections and serializers — which is where
//! the lifts have in fact landed.

use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fauna_credential_store::CredentialStore;
use fauna_protocol::{PushEvent, StaleSurfaces};
use fauna_ws_substrate::supervisor::ConnectionState;

use crate::pages::Page;

/// Backend → UI message (the linux taxonomy: Data / Noop).
///
/// `Action(ActionResult)` and `Realtime(RealtimeEvent)` were placeholders held
/// open since M0 "so the taxonomy is the linux one from day one". Five
/// milestones later nothing had ever constructed either — both wrapped
/// uninhabited enums — and the shape the pages actually converged on is
/// `Data(DataMessage::Page(..))`. Dropped 2026-07-16; trivially re-addable if a
/// real payload ever needs them.
// Boxing `Data` to close the large_enum_variant gap would touch all 30+
// UiMessage::Data construction/match sites across the crate for a one-time
// ~200-byte difference — not worth the churn for a lint, unlike a real hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum UiMessage {
    Data(DataMessage),
    /// Heartbeat / keep-alive — no state update needed.
    #[allow(dead_code)]
    Noop,
    /// The `barrier` self-test probe's work item
    /// ([`fauna_e2e_agent::BARRIER_PROBE`]). Rides this channel precisely because
    /// it is the channel real backend work rides — a probe on a private side
    /// channel would prove the barrier drains the side channel and nothing else.
    /// Applying it publishes the token at `state.barrier_probe`.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    BarrierProbe(String),
}

/// Backend data updates. At M0 only the connection state exists; feed posts,
/// messages, contacts, … arrive with their pages (M2+).
#[derive(Debug)]
pub enum DataMessage {
    ConnectionState(ConnectionState),
    /// The shared `NestClient` reconnected (a `Connected` after the first —
    /// `subscribe_reconnects`). Re-pull every visible snapshot surface: the push
    /// `seq` resets to 0 on reconnect, so anything the nest tried to broadcast
    /// while the socket was down is gone and only a pull recovers it, and the
    /// feed has no poll backstop (`transport.md` § Push events, Track-2). Carries
    /// no payload — the observer-tick shape (`FeedChanged`): the refetches read
    /// live client state.
    Reconnected,
    /// A server-initiated push arrived on the one authenticated socket
    /// (`subscribe_pushes`). Dispatched per kind by the shared [`PushEvent::invalidates`] into the
    /// snapshot refetches it implies — the tui twin of linux's central
    /// `handle_ws_event` `WsEvent::Push` match (`transport.md` § Push events).
    Push(PushEvent),
    /// The onboarding machine mutated — re-read its snapshots and redraw. The
    /// tick carries no payload (the observer contract: "the observer reads
    /// fresh snapshots via the machine's getters").
    WizardChanged,
    /// The post-auth `fauna.spam.get_preferences` read landed — the viewer's OWN
    /// spam/phishing thresholds, which the content-policy engine composes into
    /// every feed and conversation render (`family-safety.md` § Content policy:
    /// the own-threshold collapse is built "once, for every user", supervised or
    /// not). The tui twin of linux's `DataMessage::SpamPreferencesLoaded`.
    SpamPreferencesLoaded(fauna_client_spam::spam::SpamPreferences),
    /// The region relay's answers for the declared chain
    /// (`crate::region::spawn_refresh`), folded on the UI thread by
    /// `crate::region::apply_replies`. A failed ask rides as its `Err` and
    /// writes nothing.
    RegionReplies(
        Vec<(
            fauna_core::region_authority::RegionCode,
            Result<fauna_protocol::region::RegionArtifactGetReply, String>,
        )>,
    ),
    /// The aftermath's `NestBackupKey` leg reported in — fired by the
    /// post-store-ready pass, which reads this box's list from
    /// `fauna.state.backup`. Same rule as the arm above: the projection
    /// travels, not the outcome.
    BackupRegrantProgress(fauna_client_config::BackupRegrantProgress),
    /// The aftermath's `__mls` leg — the successor's conversations being
    /// unlocked under the new identity (`succession-aftermath.md` § Re-key
    /// scope, the `BackupKey` corpus row).
    MlsResealProgress(fauna_client_mls_sync::ReplicaResealProgress),
    /// The aftermath's capability-grant leg — the trust the owner had given to
    /// services, re-minted under the successor (`succession-aftermath.md`
    /// § Re-key scope, the capability-grants row). The projection travels for
    /// the same reason as the arms above; note its *adjudication* half does not
    /// come through here at all — every re-minted grant is marked at rest, and
    /// the mark renders on the Nests page.
    GrantRemintProgress(fauna_client_capabilities::GrantRemintProgress),
    /// The aftermath's **file corpus** leg (`succession-aftermath.md` § Re-key
    /// scope, the `BackupKey` corpus row).
    ///
    /// ⚠ The only aftermath arm whose work does not run in this process: the
    /// re-seal pass runs in the per-user `fauna-sync-agent`, so this arrives
    /// from `crate::sync_agent`'s status poll folding the agent's `ListEngines`
    /// roster, not from a task this app spawned. It therefore arrives
    /// **repeatedly** as the pass progresses, where the four legs above arrive
    /// once or twice; each message is the whole current reading.
    CorpusResealProgress(fauna_client_sync::agent::CorpusResealProgress),
    /// The aftermath's **mail** leg — every pre-succession credential revoked
    /// and the MSEK rotated (`succession-aftermath.md` § Re-key scope, the MSEK
    /// row). The projection travels for the same reason as the arms above, and
    /// it matters most here: this is the one leg whose success line has to tell
    /// the user their mail apps just stopped working *on purpose*.
    MailBurnProgress(fauna_client_mail_settings::MailBurnProgress),
    /// The aftermath's **drafts** leg — every `__drafts` rail re-sealed under
    /// the successor's own key (`succession-aftermath.md` § Re-key scope, the
    /// `BackupKey` corpus row, which names `__drafts` explicitly). The
    /// projection travels for the same reason as the arms above; like the
    /// `__mls` leg it is multi-unit, so its partly-owed arm is an ordinary
    /// outcome rather than an edge case.
    DraftsResealProgress(fauna_client_drafts::DraftsResealProgress),
    /// The open unattested-member review roster, freshly read from the succession ledger
    /// (`identity-succession.md` § Propagation → *MLS groups*).
    ///
    /// Unlike the five arms above this is **not** a progress reading — it is the
    /// state two surfaces render, so it arrives whenever it could have changed:
    /// once at the post-auth hook (after the aftermath has had its chance to
    /// raise items) and again after every adjudication. Carries the whole
    /// roster, never a delta: the merge that decides what is open runs nest-side
    /// in `save_cas`, so a delta computed here could disagree with it.
    MemberReviews(Vec<fauna_core::data::MemberReview>),
    /// The ids of email filter rules a succession carried across that are still
    /// un-adjudicated, freshly read from the succession ledger
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across, the fourth plane). The member roster's twin above, arriving at the
    /// same two moments and carrying the whole list for the same reason.
    FilterMarks(Vec<i64>),
    /// The `LaunchMachine` settled on a phase. Carries the snapshot rather than
    /// a bare tick because the routing decision consumes `phase` + `last_error`
    /// atomically — re-reading the machine could observe a later transition.
    LaunchPhase(fauna_launch_machine::LaunchSnapshot),
    /// The `LaunchMachine` changed state **on its own** — the tick its
    /// observer relays (`launch::SnapshotRelay`). Payload-free, unlike
    /// [`Self::LaunchPhase`]: nothing settled is being handed over, so the
    /// handler re-reads the machine. Only the locked notice acts on it
    /// (`crate::locked::follow_snapshot`): the machine runs the one refresh at
    /// a lock's unlock time itself, and this is how the surface hears it
    /// (`devices.md` § The locked state).
    LaunchMachineChanged,
    /// The locked surface's stolen-identity ceremony finished on the keyboard
    /// path (`crate::locked::spawn_submit`). Boxed: the outcome carries a seed
    /// and a sweep report.
    LockedCeremony(Box<crate::settings::StolenOutcome>),
    /// A background silent refresh met `fauna.auth.account_locked`: the
    /// account was locked while this session was running (`devices.md` § The
    /// locked state). Escalates to the launch flow, whose re-run challenge
    /// earns the same refusal and lands the locked surface on a live machine.
    AccountLocked,
    /// A **post-auth** background silent challenge met a nest whose pinned
    /// deployment identity it can no longer prove (`security.md` § Post-auth
    /// surfacing, ratified 2026-07-23). The handler routes it to the SAME
    /// blocking `launch_identity_changed` surface the launch path renders —
    /// never a banner, badge or toast, and deliberately not the error path: a
    /// possible-MITM signal is not a fault the session can survive.
    ///
    /// **Payload-free on purpose** (linux's is too, `apps/fauna-linux/src/app.rs:73`).
    /// A payload would invite rendering the surface from a synthesized phase,
    /// whose re-trust button would have no live machine to drive and would
    /// silently do nothing — the dead-button class the launch path already paid
    /// for once. The handler re-enters the real launch flow instead.
    NestIdentityChanged,
    /// A background silent refresh landed handle/domain in the account registry
    /// (`session::silent_refresh` → `update_cache`). The handler pushes the
    /// newly-resolved `<handle>@<domain>` into the conversations session's live
    /// self-address cell (`ConversationsSession::set_self_address`), healing
    /// the SMTP `From:`, the MLS same-nest routing domain, and the reply-all
    /// self-drop in one call (`conversations.md` § State & data shape →
    /// *Self-address: live, never baked*). Payload-free: the handler re-reads
    /// the registry, the single source the cache write just updated.
    SelfAddressRefreshed,
    /// A background silent refresh learned this identity was **succeeded**
    /// (`identity-succession.md` § Propagation → *Own device fleet*). Escalates
    /// to the launch flow, which lands on the identity-import screen.
    IdentitySuperseded,
    /// The nest stopped signing this signed-in identity in
    /// (`fauna.auth.not_registered`) — the user was suspended or removed
    /// mid-session. Met by the reconnect supervisor's post-4401 re-mint or by a
    /// background silent refresh. Escalates to the launch flow, whose re-run
    /// challenge earns the same refusal and lands the previously-signed-in
    /// row's surface (`onboarding.md` § App-launch routing: "the same verdict
    /// mid-session lands the same surface"), Retry live on its machine.
    SignInRefused,
    /// The claimed successor was **verified** against the registration chain
    /// (`fauna_client_recovery::resolve_successor`), so the superseded screen may
    /// now name it: the message upgrades from the claim-free
    /// `identity_superseded` to `identity_superseded_verified`.
    ///
    /// Carried as its own message because the verification is a *later*,
    /// best-effort round trip over an anonymous connection — the screen renders
    /// immediately from the refusal, and this only ever upgrades what it says.
    /// A failed, unreachable or non-contiguous verify simply never arrives, and
    /// the claim-free message stands: the nest is an enforcer and distributor,
    /// never an authorizer, so an unproven successor is never presented as fact.
    ///
    /// The payload is the successor's **public** `ActorId` hex — for display
    /// only. It is deliberately *not* fed to `paste-secret-field`, which takes
    /// the successor's secret seed: an `ActorId` is also 64 hex characters, so it
    /// would pass the import's format check and be adopted as an identity secret,
    /// silently creating an account nobody holds the key to.
    ///
    /// `predecessor` is the refused identity's actor id hex — the link
    /// [`App::adopt_held_successor`] records when this device turns out to
    /// hold the successor's key already.
    IdentitySupersededVerified {
        successor: String,
        predecessor: String,
    },
    /// The `recover-selfhosted-command` resolved off a reachable surviving nest
    /// (`box-recovery.md` § Recovery UI (step 4)). Carried as a message because
    /// the machine deliberately surfaces no seed — this is the one sanctioned
    /// exception, and it lands on the `Wizard`, not the machine.
    RecoverySelfhostedCommand(String),
    /// The custodied boxes the launch-time read found (`box-recovery.md`
    /// § Recovery UI (step 4) — the surviving-device entry). A non-empty list is
    /// what reveals `launch-recover-button` on the transient-retry surface.
    LaunchRecoverBoxes(Vec<String>),
    /// A deployment-seed custody leg ended with an admin's custody unconfirmed
    /// (`box-recovery.md` § The plane-era recovery floor, (c) The writes) —
    /// the text is the shared `fauna_client_config::custody_leg_warning`.
    /// Surfaced onto the shared `warning-message` element (`app.injected_warning`)
    /// so the admin learns total-box-loss recovery isn't protected rather than
    /// discovering it only at box loss — tui's idiom of linux's
    /// `ActionResult::Failed` toast. Carried as a message because the leg is a
    /// fire-and-forget bg task (`crate::recovery::spawn_custody_leg`).
    RecoveryCustodyWarning(String),
    /// The `FeedManager` mutated — re-read its snapshot and redraw. Like
    /// `WizardChanged` the tick carries no payload: the observer contract is
    /// "read a fresh snapshot off the manager", so coalescing duplicate ticks
    /// loses nothing.
    FeedChanged,
    /// The `ConversationsManager` mutated — re-read its snapshot and redraw. Like
    /// `FeedChanged` the tick carries no payload: the observer contract is "read a
    /// fresh snapshot off the manager", so coalescing duplicate ticks loses
    /// nothing.
    ConversationsChanged,
    /// The `MediaMachine` mutated — re-read its snapshot and redraw. Same
    /// payload-free observer tick as `FeedChanged` / `ConversationsChanged`.
    MediaChanged,
    /// The `SearchManager` mutated — re-read its snapshot and redraw. Payload-free
    /// like the ticks above. Load-bearing beyond the redraw: the manager opens a
    /// query (`in_flight`) *before* awaiting either backend, so without this tick
    /// the searching state would never paint, and a failure the manager records
    /// would not reach `error-message` until the op it belongs to settled.
    SearchChanged,
    /// A teardown's spawned stop ended — the sync agent's un-provision, the
    /// account runtime's stop and the session client's disconnect
    /// (`session::sign_out`). Releases whatever [`App::after_stops`] queued
    /// behind it: the erase, the next session's launch, a deferred e2e ack.
    AccountRuntimeStopped,
    /// The account-store runtime finished assembling (`session::establish`
    /// spawns the assembly: open the store, mint-or-load the writer key, start
    /// the pump — `account-data-plane.md` § The client-side lifecycle). The
    /// handle lands on `SettingsState.account_store`, and the preference
    /// gestures waiting on it proceed. Carries the actor id
    /// so a handle whose session was signed out (or switched) while the
    /// assembly ran is shut down instead of installed — a stale runtime
    /// pumping the wrong account's scopes would be a silent cross-account
    /// leak of exactly the kind the per-account store placement exists to
    /// prevent.
    AccountStoreReady {
        actor_id: String,
        handle: fauna_sync_engine::account_runtime::AccountStoreHandle,
    },
    /// The share glue bound — or was handed — this session's seat
    /// (`share_glue::run`, under rule 7's live-else-cached brake).
    /// Payload-free: the seat already sits in
    /// `SettingsState.offline_share.session_seat`, the slot both bind doors go
    /// through (`offline_share::SessionSeat` — one endpoint per session by
    /// construction), so there is nothing to fold and no stale seat to guard
    /// against.
    #[cfg(feature = "p2p-share")]
    SharePlaneSeatBound,
    /// A share pump pass changed the transfer surface's state cell
    /// (`share_glue::SharePlaneState`). Payload-free like
    /// `SyncAgentChanged`: the paint reads the fresh cell on the redraw this
    /// message triggers; posted only on actual change.
    #[cfg(feature = "p2p-share")]
    SharePlaneChanged,
    /// The account store's cross-connection change counter moved — a sibling
    /// same-account process committed to the shared store (the W5.2 (account-data-plane.md § Workstreams)
    /// `data_version` poll floor, `account-data-plane.md` § Multi-instance
    /// concurrency). Posted by [`crate::session::account_store_watch`] only on
    /// actual movement — a runtime's own writes never move its own counter,
    /// so a single-instance deployment never wakes the loop. Payload-free
    /// like `SyncAgentChanged`: the handler re-drives the open surface's own
    /// load — reload semantics, the same refetch a same-page nav fires.
    AccountStoreChanged,
    /// The sync agent became reachable — the convergence loop's false→true edge
    /// (`fauna_client_sync::agent::ReachabilityObserver`). Re-drive the
    /// folder-binding reconcile against the agent's `ListLocations` truth (never
    /// only once at attach — the A4 attach-race fix). Carries no payload: the
    /// handler reads the installed surface off `App` (the tui idiom of linux's
    /// `glib::idle_add_once` → thread-local `AGENT` indirection).
    SyncAgentReconcile,
    /// ACCOUNT-custody ceremony state advanced (an offer / accept / deliver /
    /// A7 receipt was captured on the conversations poll path — T16, a
    /// different plane from folder content keys): the
    /// handler runs one background `drive_ceremonies` pass
    /// (`custody_glue::spawn_drive`), the record-then-act "act" half.
    CustodyCeremonyMoved,
    /// The sync-agent folder model's rendered binding set changed (a reconcile
    /// adopted or pushed a binding) — re-read + redraw, the payload-free observer
    /// tick shape (`FeedChanged` / `MediaChanged`).
    SyncAgentChanged,
    /// A user's bind of a location under the named set confirmed agent-side.
    /// The agent's bind enrols this device's place when it held none
    /// (`file-sync.md` § 4, *A local presence writes the place it needs*), so
    /// an expanded row for that set re-reads its roster
    /// (`crate::settings::roster_resync_op`) and the new seat paints.
    FolderBindConfirmed(String),
    /// The local sync-agent's health reading moved (`sync-agent.md` § Local agent
    /// health) — the `sync-agent-status` shell line has new text. Posted by
    /// `sync_agent`'s 10 s `GetServiceStatus` poll, and only on an actual change,
    /// so a steady-state agent never wakes the loop. Payload-free like
    /// `SyncAgentChanged`: the paint reads the fresh value off
    /// `SyncAgentState::rendered_status`.
    SyncAgentStatusChanged,
    /// **Any** page's network half resolved — the one variant every page's
    /// `Outcome` rides home on ([`PageOutcome`]).
    ///
    /// This is the keyboard path's half of the actuation duality: it spawned the
    /// [`PageOp`] and the outcome comes back here to be folded. The agent's path
    /// awaits the same op and folds the same outcome directly, before its
    /// single-shot reply ([`crate::automation::run_gesture`]). One variant, not
    /// one per page: a new page costs a `PageOutcome` variant, not another arm
    /// here.
    ///
    /// Non-gesture producers (the login/nav refetches in `contacts`,
    /// `notifications`, `profile`, `settings`) send through this variant too.
    ///
    /// Carries the [`App::session_generation`] the producer captured at spawn
    /// (before its first await), checked against the current one at the single
    /// fold arm ([`App::handle_message`]'s `Page` arm) — the generic form of the
    /// identity seam (`account-scoping.md` § The scoping taxonomy → the
    /// in-memory corollary): a page op holds no cancellation handle, so a result
    /// already in flight when an actor change lands still resolves, and without
    /// the stamp it would fold into whatever page state `session::establish` has
    /// since installed for the INCOMING actor. One field on the envelope covers every page — a new
    /// one costs nothing here, unlike a per-`Outcome`-variant field hand-copied
    /// onto each new variant.
    Page(u64, PageOutcome),
}

/// The network half of a gesture, for **every** page — one type so the two
/// actuation paths need one `run()` and one fold.
///
/// Each variant owns only `Arc`s, so it crosses a `tokio::spawn`.
pub enum PageOp {
    Feed(crate::feed::Op),
    Conversations(crate::conversations::Op),
    Contacts(crate::contacts::Op),
    Notifications(crate::notifications::Op),
    Moderation(crate::moderation::Op),
    Report(crate::report::Op),
    Profile(crate::profile::Op),
    Events(crate::events::Op),
    Settings(crate::settings::Op),
    Search(crate::search::Op),
    Media(crate::media::Op),
    Admin(crate::admin::Op),
    Nostr(crate::nostr::Op),
    Bridges(crate::bridges::Op),
    Family(crate::family::Op),
    Backups(crate::backups::Op),
}

/// What a [`PageOp`] resolved to — the page's own `Outcome`, boxed into one type.
#[derive(Debug)]
pub enum PageOutcome {
    Feed(crate::feed::Outcome),
    Conversations(crate::conversations::Outcome),
    Contacts(crate::contacts::Outcome),
    Notifications(crate::notifications::Outcome),
    Moderation(crate::moderation::Outcome),
    Report(crate::report::Outcome),
    Profile(crate::profile::Outcome),
    Events(crate::events::Outcome),
    Settings(crate::settings::Outcome),
    Search(crate::search::Outcome),
    Media(crate::media::Outcome),
    Admin(crate::admin::Outcome),
    Nostr(crate::nostr::Outcome),
    Bridges(crate::bridges::Outcome),
    Family(crate::family::Outcome),
    Backups(crate::backups::Outcome),
}

/// Spawn a nav-enter-style refresh: run `$op_expr` (an `Option<Op>`) if
/// present, folding its outcome home as one [`PageOutcome::$Variant`] —
/// the shape `contacts::spawn_refresh`, `notifications::spawn_refresh`,
/// `events::spawn_refresh` and `settings::spawn_quota_refresh` each hand-copied
/// (round 201 of the shared-Rust lift sweep). Not [`App::spawn_page_op`] itself: each of those call sites only
/// has `&XState` + the channel in hand (page `init` runs before `App` exists;
/// `SettingsState::attach_session` runs as `&mut self`), never `&App`. `$op_expr`
/// is taken as a caller expression (not called from inside this macro) so it
/// resolves against the *invocation* site's own `nav_enter_op`, not this
/// module's. `$generation` is the caller's own `App::session_generation`,
/// captured on its synchronous path — the [`DataMessage::Page`] identity seam.
macro_rules! spawn_nav_refresh {
    ($op_expr:expr, $tx:expr, $generation:expr, $Variant:ident) => {{
        let Some(op) = $op_expr else {
            return;
        };
        let tx = $tx.clone();
        let generation = $generation;
        tokio::spawn(async move {
            let outcome = op.run().await;
            let _ = tx.send($crate::app::UiMessage::Data(
                $crate::app::DataMessage::Page(
                    generation,
                    $crate::app::PageOutcome::$Variant(outcome),
                ),
            ));
        });
    }};
}
pub(crate) use spawn_nav_refresh;

impl PageOp {
    /// Whether this op's work deliberately **outlives the click that started
    /// it**, so the agent path must spawn it rather than await it.
    ///
    /// Awaiting is the agent's default and stays that way (`automation.rs`
    /// § the actuation duality): element reads are single-shot, so a click's
    /// effect must be folded before `/element/click` replies. That argument
    /// only holds for ops whose result the *next read* is supposed to see.
    ///
    /// A factory reset is the opposite. Its success outcome is a whole-app
    /// transition (`enter_factory_reset_reonboard`), not a value the click
    /// reads back, and every driver-side consumer already polls for the
    /// resulting surface rather than trusting the click's return
    /// (`actions/admin.py::factory_reset_via_ui` waits for `claim-code-input`;
    /// the crash-recovery journeys poll a nest-log beacon or SIGKILL at once).
    /// Awaiting it also makes the click hostage to a nest that may be *gone by
    /// design*: against an unreachable nest the op's two sequential RPCs each
    /// run out the client-side spec default of 30 s
    /// (`fauna-client::client::request_inner` — the per-kind registry is not
    /// wired up in production, `fauna-protocol::kind` § the `register_*_kinds`
    /// caveat), so the click blocks ~60 s and no agent reply budget under the
    /// driver's own 30 s POST timeout can absorb it.
    ///
    /// This is not a test-only bypass, and it is not a divergence: linux's
    /// production entry point is already fire-and-forget by construction
    /// (`apps/fauna-linux/src/client.rs::factory_reset` is a **sync** fn that
    /// `spawn_bg`s), which is exactly why these journeys have always been green
    /// there. tui's keyboard path likewise already spawns
    /// ([`App::spawn_gesture`]); this makes the agent path agree with both,
    /// the same reasoning `automation.rs` records for `start_provisioning`.
    ///
    /// **The succession ceremony joins for a stronger version of the same
    /// reason: its RPC count scales with the USER's group count.** After the
    /// account moves, `sweep_after_succession` runs add-successor → publish →
    /// join → remove-old over *every group the user holds*, so the op's
    /// duration is bounded by their conversation list and nothing else. That is
    /// not a budget any agent reply ceiling can be sized against — a reply
    /// window wide enough for a hundred-group account is one no failing run
    /// would ever return from — and unlike `FactoryReset` there is no fixed
    /// worst case to pick. On top of that the ceremony **revokes the driving
    /// session's own bearer and then switches accounts**, so awaiting it holds
    /// the HTTP reply open across a session teardown and relaunch: the reply
    /// would be owed by a session that no longer exists.
    ///
    /// Same three properties as the arm above, so this is likewise no
    /// divergence: tui's keyboard path already spawns
    /// ([`App::spawn_gesture`]), the outcome still folds through
    /// [`apply_page_outcome`] via [`Self::spawn_page_op`]'s channel — so the
    /// switch and the sweep report land exactly as before — and every
    /// driver-side consumer already polls the resulting surface rather than the
    /// click (`actions/settings.py::succeed_identity_with_held_kit` says so in
    /// its own docstring: *"a caller waits on the new signed-in surface"*).
    /// Found by the tier_3 sweep journey: with one real group the ceremony
    /// overran the agent's 25 s reply budget, while the zero-group sibling
    /// passed — i.e. the budget was only ever green because nothing was swept.
    pub fn outlives_click(&self) -> bool {
        matches!(
            self,
            PageOp::Admin(crate::admin::Op::FactoryReset { .. })
                // The committed rotation tears down the box's serving generation
                // (`box-recovery.md` § Adoption by the running process), so this
                // drive's own marking step reconnects *by design* — bounding it
                // by the agent's reply budget would abandon the ceremony between
                // its dispatch and its bookkeeping, which is the one window
                // where the admin most needs its verdict.
                | PageOp::Admin(crate::admin::Op::RotateDeploymentSeed { .. })
                | PageOp::Settings(crate::settings::Op::RecoveryStolen { .. })
        ) || self.waits_on_the_other_person()
    }

    /// The co-present ceremony's two blocking halves, split out because their
    /// reason differs from the three above: they do not outlive the click
    /// because they are long, but because **they are waiting on another
    /// human** — the initiator's Begin holds until the recipient's user taps
    /// Accept (up to `CONSENT_BUDGET`), and that Accept holds until the
    /// initiator's deliver crosses. Awaiting either would make one person's
    /// UI unresponsive for as long as the other takes to decide, which is not
    /// a budget problem but a wrong shape. Both consumers poll the surface
    /// (`offline-share-status`, the folders listing), never the click.
    fn waits_on_the_other_person(&self) -> bool {
        #[cfg(feature = "p2p-share")]
        {
            matches!(
                self,
                PageOp::Settings(crate::settings::Op::BeginOfflineShare(_))
                    | PageOp::Settings(crate::settings::Op::AcceptGroupShare(_))
            )
        }
        #[cfg(not(feature = "p2p-share"))]
        {
            false
        }
    }

    /// Run the network half. Owns only `Arc`s, so both paths can drive it: the
    /// keyboard's `tokio::spawn`, and the agent's direct await.
    ///
    /// Every arm **boxes** its nested future rather than awaiting it inline.
    ///
    /// An `async fn`'s state machine is the sum of its await points, and rustc
    /// does not overlap them across match arms in a debug build — so awaiting
    /// 15 page ops inline embedded all 15 of their state machines (the largest,
    /// `settings::Op::run`, is a 73-variant async match) into *this* future,
    /// and thence into every caller's. Measured on Windows-arm64: the nav chain
    /// peaked at 837 KiB of main-thread stack, which overflows Windows' 1 MiB
    /// reserve and killed the app on any Settings nav. Boxing each arm leaves a
    /// pointer here instead of a state machine and cut the peak to 630 KiB —
    /// the whole chain above shrank with it (`apply_nav` 309 -> 177 KiB).
    ///
    /// Keep this shape when adding a page: `Box::pin(op.run()).await`, never a
    /// bare `op.run().await`. See `build.rs` for the platform-reserve half.
    pub async fn run(self) -> PageOutcome {
        match self {
            PageOp::Feed(op) => PageOutcome::Feed(Box::pin(op.run()).await),
            PageOp::Conversations(op) => PageOutcome::Conversations(Box::pin(op.run()).await),
            PageOp::Contacts(op) => PageOutcome::Contacts(Box::pin(op.run()).await),
            PageOp::Notifications(op) => PageOutcome::Notifications(Box::pin(op.run()).await),
            PageOp::Moderation(op) => PageOutcome::Moderation(Box::pin(op.run()).await),
            PageOp::Report(op) => PageOutcome::Report(Box::pin(op.run()).await),
            PageOp::Profile(op) => PageOutcome::Profile(Box::pin(op.run()).await),
            PageOp::Events(op) => PageOutcome::Events(Box::pin(op.run()).await),
            PageOp::Settings(op) => PageOutcome::Settings(Box::pin(op.run()).await),
            PageOp::Search(op) => PageOutcome::Search(Box::pin(op.run()).await),
            PageOp::Media(op) => PageOutcome::Media(Box::pin(op.run()).await),
            PageOp::Admin(op) => PageOutcome::Admin(Box::pin(op.run()).await),
            PageOp::Nostr(op) => PageOutcome::Nostr(Box::pin(op.run()).await),
            PageOp::Bridges(op) => PageOutcome::Bridges(Box::pin(op.run()).await),
            PageOp::Family(op) => PageOutcome::Family(Box::pin(op.run()).await),
            PageOp::Backups(op) => PageOutcome::Backups(Box::pin(op.run()).await),
        }
    }
}

/// Fold a resolved [`PageOutcome`] back onto its page — the one fold both
/// actuation paths share.
pub fn apply_page_outcome(app: &mut App, outcome: PageOutcome) {
    match outcome {
        PageOutcome::Feed(o) => crate::feed::apply_outcome(app, o),
        PageOutcome::Conversations(o) => crate::conversations::apply_outcome(app, o),
        PageOutcome::Contacts(o) => crate::contacts::apply_outcome(app, o),
        PageOutcome::Notifications(o) => crate::notifications::apply_outcome(app, o),
        PageOutcome::Moderation(o) => crate::moderation::apply_outcome(app, o),
        PageOutcome::Report(o) => crate::report::apply_outcome(app, o),
        PageOutcome::Profile(o) => crate::profile::apply_outcome(app, o),
        PageOutcome::Events(o) => crate::events::apply_outcome(app, o),
        PageOutcome::Settings(o) => crate::settings::apply_outcome(app, o),
        PageOutcome::Search(o) => crate::search::apply_outcome(app, o),
        PageOutcome::Media(o) => crate::media::apply_outcome(app, o),
        PageOutcome::Nostr(o) => crate::nostr::apply_outcome(app, o),
        PageOutcome::Admin(o) => crate::admin::apply_outcome(app, o),
        PageOutcome::Bridges(o) => crate::bridges::apply_outcome(app, o),
        PageOutcome::Family(o) => crate::family::apply_outcome(app, o),
        PageOutcome::Backups(o) => crate::backups::apply_outcome(app, o),
    }
}

/// Whether a push has staled the surface tui serves from its contacts op.
///
/// The one place tui's page inventory differs from the shared vocabulary:
/// [`StaleSurfaces`] separates pending knocks from the roster because linux
/// re-reads them independently, while tui's `contacts::nav_enter_op` fetches
/// both in one pass — so either flag means exactly one refresh here.
fn contacts_stale(r: &StaleSurfaces) -> bool {
    r.knocks || r.contacts
}

/// What a gesture implies **beyond** its synchronous local half — the return of
/// the one gesture door, [`gesture_work`].
///
/// This is the type that makes the door "parameterized on await-vs-spawn"
/// (`apps/tui.md` § Architecture § Target state): the door applies every
/// gesture's local half and hands back only the work that still has to run, and
/// the *caller* decides how to run it. The keyboard spawns (a slow nest must
/// never freeze the render loop) and the agent awaits (element reads are
/// single-shot, so the effect must land before the reply). Neither one re-derives
/// which page a gesture belongs to.
pub enum GestureWork {
    /// Fully local — nav, opening a cached detail, a draft write. Nothing to run.
    None,
    /// A page's network half.
    Page(Box<PageOp>),
    /// The onboarding machine's async mutator. Not a [`PageOp`]: it has no
    /// `Outcome` to fold — the machine's observer tick is what drives the redraw.
    Wizard(
        std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>,
        crate::wizard::Action,
        String,
    ),
    /// The launch surface's action. Not a [`PageOp`] either: it drives the
    /// `LaunchMachine` and owns its own spawn/await pair.
    Launch(crate::launch::LaunchAction),
    /// The `nest_retire` page's machine call (`crate::wizard::nest_retire`) —
    /// it redraws itself when it lands.
    Retire(crate::wizard::nest_retire::RetireWork),
}

impl From<Option<PageOp>> for GestureWork {
    fn from(op: Option<PageOp>) -> Self {
        match op {
            Some(op) => GestureWork::Page(Box::new(op)),
            None => GestureWork::None,
        }
    }
}

/// **The one gesture door.** Apply a gesture's synchronous local half and hand
/// back whatever still has to run.
///
/// Both actuation paths go through here — `App::spawn_gesture` (keyboard) and
/// [`crate::automation::run_gesture`] (the e2e agent) — so the per-page dispatch
/// exists exactly once. It used to be hand-written three times (the keyboard
/// spawn, the agent's Click arm, and a partial third copy in its Select arm),
/// which is why a page's select could reach a dispatch table that never listed
/// it and get acked green having done nothing (testing.md point 10). The match
/// below is exhaustive with no `_` arm: a new `Gesture` variant cannot be added
/// without deciding, here and once, what it runs.
pub fn gesture_work(app: &mut App, gesture: crate::element::Gesture) -> GestureWork {
    use crate::element::Gesture;
    let work = match gesture {
        // --- the machine-driven surfaces (not the page-Op contract) ---
        // `dispatch_wizard` validates client-side and yields the payload; the
        // caller runs `wizard::run_action` (spawned or awaited).
        Gesture::Wizard(action) => {
            let mut pending = None;
            app.dispatch_wizard(action, |machine, action, payload| {
                pending = Some((machine, action, payload));
            });
            match pending {
                Some((machine, action, payload)) => GestureWork::Wizard(machine, action, payload),
                // `prepare` rejected it — the error is on the wizard already.
                None => GestureWork::None,
            }
        }
        Gesture::Launch(action) => GestureWork::Launch(action),
        // The local half (open/close, select, the typed gate's buffer, a copy)
        // is applied here; what comes back is the machine call still to run.
        Gesture::Retire(action) => match crate::wizard::nest_retire::gesture(app, action) {
            Some(work) => GestureWork::Retire(work),
            None => GestureWork::None,
        },
        // Synchronous on BOTH paths by design (`crate::unlock` module docs): the
        // Argon2id derive is ~100ms and both paths must observe the settled
        // surface, so there is no work left to hand back.
        Gesture::Unlock(crate::unlock::UnlockAction::Submit) => {
            let tx = app.tx.clone();
            crate::unlock::submit(app, &tx);
            GestureWork::None
        }
        // Synchronous on both paths, for the unlock submit's reason: the agent
        // must observe the settled outcome (the view gone, or its new line)
        // in the click's own reply.
        Gesture::RetrySignOutResidue => {
            crate::account_scope::retry_residue(app);
            GestureWork::None
        }

        // --- purely local: no network half on either path ---
        // `Page::Exit` is a sidebar row, not a destination (`Page::Exit`'s doc
        // comment) — actuating it quits instead of "entering" the empty pane
        // it would otherwise paint. Both actuation paths reach here (this
        // function's own doc comment), so this is the one place that decides
        // it, same as every other `Gesture::Nav` target.
        Gesture::Nav(Page::Exit) => {
            app.should_quit = true;
            GestureWork::None
        }
        // The nav-edge refresh rides home exactly like any other page op.
        Gesture::Nav(page) => app.apply(page).into(),
        Gesture::OpenProfile(target) => {
            app.open_profile(target);
            GestureWork::None
        }
        // The row is already cached — unlike `SelectCalendar`, which refetches.
        Gesture::OpenEventDetail(event_id) => {
            crate::events::open_event_detail(app, event_id);
            GestureWork::None
        }

        // --- the pages: one line each, the whole point of the door (the gated
        // admin shell among them) ---
        Gesture::Feed(a) => crate::feed::apply_local(app, a).map(PageOp::Feed).into(),
        Gesture::Conversations(a) => crate::conversations::apply_local(app, a)
            .map(PageOp::Conversations)
            .into(),
        Gesture::Contacts(a) => crate::contacts::apply_local(app, a)
            .map(PageOp::Contacts)
            .into(),
        Gesture::Moderation(a) => crate::moderation::apply_local(app, a)
            .map(PageOp::Moderation)
            .into(),
        Gesture::Report(a) => crate::report::gesture(app, a).into(),
        Gesture::Notifications(a) => crate::notifications::apply_local(app, a)
            .map(PageOp::Notifications)
            .into(),
        Gesture::Profile(a) => crate::profile::apply_local(app, a)
            .map(PageOp::Profile)
            .into(),
        Gesture::Events(a) => crate::events::apply_local(app, a)
            .map(PageOp::Events)
            .into(),
        Gesture::Settings(a) => crate::settings::apply_local(app, a)
            .map(PageOp::Settings)
            .into(),
        Gesture::Search(a) => crate::search::apply_local(app, a)
            .map(PageOp::Search)
            .into(),
        // Carries its own target (like `OpenProfile`/`SelectCalendar`) because
        // the destination is a different page's op — `open_result` wraps it
        // back into the SAME `PageOp` variant a direct click on that page
        // would produce, so a gated post's unseal still runs.
        Gesture::OpenSearchResult(nav) => crate::search::open_result(app, nav).into(),
        // The `OpenSearchResult` twin: carries its own target because the
        // destination is a different page's op, and `open_notification` wraps
        // it back into the SAME `PageOp` variant a direct click on that page
        // would produce — so a deep-linked post still resolves and unseals.
        Gesture::OpenNotification(dest) => {
            crate::notifications::open_notification(app, dest).into()
        }
        Gesture::Media(a) => crate::media::apply_local(app, a).map(PageOp::Media).into(),
        Gesture::Nostr(a) => crate::nostr::apply_local(app, a).map(PageOp::Nostr).into(),
        Gesture::Bridges(a) => crate::bridges::apply_local(app, a)
            .map(PageOp::Bridges)
            .into(),
        Gesture::Admin(a) => crate::admin::apply_local(app, a).map(PageOp::Admin).into(),
        Gesture::Family(a) => crate::family::apply_local(app, a)
            .map(PageOp::Family)
            .into(),
        Gesture::Backups(a) => crate::backups::apply_local(app, a)
            .map(PageOp::Backups)
            .into(),
        // Carries its own target (like `OpenProfile`) so it sits outside the
        // `Action`/`apply_local` split — but it still implies the calendar's
        // event list, folded back as an events outcome like any other.
        Gesture::SelectCalendar(calendar_id) => crate::events::select_calendar(app, calendar_id)
            .map(PageOp::Events)
            .into(),
        // The visibility twin of `SelectCalendar` — same carries-its-own-target
        // shape, but no op: it only flips client-side display state, and the
        // union arm's cache already holds every owned calendar's events.
        Gesture::ToggleCalendarVisibility(calendar_id) => {
            crate::events::toggle_calendar_visibility(app, calendar_id);
            GestureWork::None
        }
        // The agenda card's inline RSVP — carries its own target like
        // `SelectCalendar`, and deliberately leaves the page where it is (the
        // quick action's whole point is not opening the detail).
        Gesture::RsvpEvent { event_id, response } => {
            crate::events::rsvp_event(app, event_id, response)
                .map(PageOp::Events)
                .into()
        }
    };
    // Every actuation through the one gesture door — the settings-nav-back
    // button, the rail `Open*` handlers, and everything else that can move
    // `(page, sub)` — passes through here before returning, so this is one of
    // the nav-edge hook's two ordinary call sites (`App::sync_recovery_message_nav_edge`'s
    // doc comment names the other, `App::handle_key`).
    app.sync_recovery_message_nav_edge();
    work
}

/// **The one nav-edge hook.** The refresh entering `page` implies, if any.
///
/// Called by [`App::apply`] on the navigation **edge** only (never per tick),
/// and only while authenticated. It returns the op instead of firing it, so the
/// caller keeps the await-vs-spawn choice — which is the whole point: a
/// fire-and-forget nav-edge fetch can land a **stale** snapshot *after* a
/// same-visit mutation and clobber it. That is not hypothetical; an early
/// events-slice cut hit exactly this — but that was *before* this await-vs-spawn duality
/// existed, when the nav-edge refresh spawned unconditionally regardless of
/// caller. The awaited shape (this function's whole point) is race-free by
/// construction under the driver's ready-ack contract — the agent path
/// always awaits the returned op to completion before its next single-shot
/// read/command, so a same-visit mutation is strictly sequenced *after* the
/// nav-edge fetch folds, never concurrent with it (the Privacy sub-page's
/// `Op::FetchPrivacy` proved the shape; Contacts/Notifications/Nostr/Admin/
/// Settings/Media all refresh on this same edge with no guard beyond it).
/// Events now rides the identical shape (`events::nav_enter_op`) — the
/// dedicated no-fetch carve-out this comment used to document is retired.
///
/// Exhaustive, no `_` arm: a new page must decide what entering it refreshes.
fn on_nav_enter(app: &mut App, page: Page) -> Option<PageOp> {
    match page {
        // No observer-backed manager, so entering the tab IS the refresh trigger
        // (`contacts.md` — fetch at post-auth + on nav-to-tab).
        Page::Contacts => crate::contacts::nav_enter_op(&app.contacts).map(PageOp::Contacts),
        Page::Notifications => {
            crate::notifications::nav_enter_op(&app.notifications).map(PageOp::Notifications)
        }
        // Entering Moderation re-reads the union: `fauna.moderation.actions`
        // plus the conversations session's post-decrypt local detections. Never
        // cached — server obligation rows can land while no client is looking
        // (an admin action, a legal takedown), and the local half grows with
        // every inbound message the receive loop decrypts.
        Page::Moderation => crate::moderation::nav_enter_op(app).map(PageOp::Moderation),
        // Hydrate the calendar list + the scoped-or-unioned events over it
        // (events.md § Implementation status today's no-selection union +
        // calendar-list hydration; see the clobber note above for why this is
        // race-free). Its sub-page/focus RESET is deliberately not here — it is
        // shell state, so it belongs with the other edge-only resets in
        // [`App::apply`], and firing it on a same-page re-enter would throw a
        // reload's caller back to List view on today.
        Page::Events => {
            crate::events::nav_enter_op(&app.events, app.settings.account_store.clone())
                .map(PageOp::Events)
        }
        // The Status landing's live quota cell.
        Page::Settings => crate::settings::nav_enter_op(&app.settings).map(PageOp::Settings),
        // Observer-backed, but the cross-set aggregate is pulled on demand rather
        // than pushed: entering the tab IS the `fauna.media.list` trigger, exactly
        // as linux refreshes on `connect_map`.
        Page::Media => crate::media::nav_enter_op(&app.media).map(PageOp::Media),
        // Entering the admin shell loads its landing dashboard (`fauna.admin.stats`),
        // the no-client-cache "re-read on entry" rule (`admin.md` § Persistence).
        Page::Admin => crate::admin::nav_enter_op(&app.admin).map(PageOp::Admin),
        // Entering the Nostr tab re-reads the bridge status + follows
        // (`fauna.bridges.{list,list_follows}`) — an AWAITED op, never a
        // fire-and-forget spawn, so a driver's next read cannot see the
        // pre-fetch frame (the one nav-edge rule).
        Page::Nostr => crate::nostr::nav_enter_op(app).map(PageOp::Nostr),
        // Entering the Bridges tab re-reads the unified bridge list + each
        // linked bridge's follows (`fauna.bridges.{list,list_follows}`) — an
        // AWAITED op like Nostr's, so a driver's next read never sees the
        // pre-fetch frame (the one nav-edge rule).
        Page::Bridges => crate::bridges::nav_enter_op(&app.bridges).map(PageOp::Bridges),
        // Entering the gated Family tab re-reads `fauna.family.status` + the
        // guardian's approvals queue — AWAITED like the two above, and never
        // skipped: the queue is nest-authoritative, so a knock or mail hold can
        // land while another page is up.
        Page::Family => crate::family::nav_enter_op(&app.family).map(PageOp::Family),
        // Entering Backups re-reads this box's `fauna.state.backup` list + the
        // nest's `fauna.backup.status` projection — AWAITED like the three
        // above. It must re-read on every entry rather than cache: the status
        // numbers advance with every nest-side coordinator pass, i.e. while no
        // client is even running (`backups.md` § Per-destination status read).
        Page::Backups => {
            crate::backups::nav_enter_op(&app.backups, app.sync_agent.custodian_store())
                .map(PageOp::Backups)
        }
        // Observer-backed, but the manager pushes only the POSTS — the sealed
        // scorers (muted keywords, trained topic factors) load inside its
        // `reload` and nowhere else, so returning to the feed after editing them
        // in Settings must re-run the query or the page renders against the
        // pre-edit filters (`crate::feed::nav_enter_op` carries the full note).
        Page::Feed => crate::feed::nav_enter_op(&app.feed).map(PageOp::Feed),
        // Observer-backed (the manager ticks drive them) or nothing to refresh.
        Page::Conversations | Page::Profile | Page::Search => None,
        // The ring can rest here (`Page::Exit`'s doc comment) — nothing to
        // refresh for an empty body.
        Page::Exit => None,
    }
}

/// Which pane of the authenticated shell owns the keyboard.
///
/// Two zones, not one flat ring: a single ring over "sidebar + page" would make
/// switching pages a Tab-through-every-post-on-the-feed, and ↑/↓ could not both
/// move between pages and move within a page. ←/→ switch panes; ↑/↓/Tab move
/// within the active one.
///
/// The zone is meaningful only while authenticated — the unauthenticated screen
/// has no sidebar (ui.yaml `navigation.hidden_when: unauthenticated`), so the
/// wizard and launch surfaces always read as [`Zone::Page`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Sidebar,
    Page,
}

/// The post-succession aftermath's per-leg progress
/// (`succession-aftermath.md` § Re-key scope). One field per leg of the ordered
/// sequence a successor's client owes.
///
/// **A bundle rather than one field (and one render parameter) per leg**, on
/// purpose: § Re-key scope names five legs and only two are built, so the
/// alternative grows three more parameters through
/// `account_elements` → `recovery_elements` on all seven apps. A new leg adds a
/// field here and an arm in the Settings render, and touches no signature.
///
/// Each leg carries the **shared projection**, never a raw outcome: what
/// renders, and which line, is decided once in `fauna-client-config` so the
/// confusable arms (owed-by-another-device is *not* damage) read identically
/// everywhere.
#[derive(Debug, Clone, Default)]
pub struct AftermathProgress {
    /// The `NestBackupKey` re-grant + the destination-registry rebuild. Runs in
    /// the post-store-ready pass (`run_ledger_aftermath`), over this box's
    /// `fauna.state.backup` list.
    pub backup_regrant: Option<fauna_client_config::BackupRegrantProgress>,
    /// The `__mls` state-replica re-seal — the conversations plane. Runs beside
    /// the other legs rather than behind them: it reads only the predecessor
    /// keys.
    pub mls_reseal: Option<fauna_client_mls_sync::ReplicaResealProgress>,
    /// The capability-grant re-mint — the trust given to services, restored
    /// under the successor. Runs in the post-store-ready pass, off the
    /// succession ledger's rows.
    pub grant_remint: Option<fauna_client_capabilities::GrantRemintProgress>,
    /// The user's own **file corpus** — files, photos and folders moved out
    /// from under the retired identity's `BackupKey`. Runs beside the other legs
    /// rather than behind them (it reads only the predecessor keys), and is the
    /// one leg whose work happens in another
    /// process: it is folded from the sync agent's `ListEngines` roster by
    /// [`crate::sync_agent`]'s status poll, so it updates while Settings is open.
    pub corpus_reseal: Option<fauna_client_sync::agent::CorpusResealProgress>,
    /// The **mail burn** — every pre-succession credential revoked and the MSEK
    /// rotated out from under the retired identity's seed holder
    /// (`succession-aftermath.md` § Re-key scope, the MSEK row). Runs last in
    /// the post-store-ready pass, off the mail custody's rows.
    ///
    /// ⚠ The one leg whose *completion* costs the user something — every mail
    /// app has to be set up again — which is why its line names that
    /// consequence rather than reporting a tidy success.
    pub mail_burn: Option<fauna_client_mail_settings::MailBurnProgress>,
    /// The user's **unsent drafts** — every `__drafts` rail re-sealed out from
    /// under the retired identity's `BackupKey`. Runs beside the other legs
    /// rather than behind them (it reads only the predecessor keys), and
    /// **before** [`Self::mail_burn`] deliberately: the
    /// burn is the only leg that takes something away, so every restoring leg
    /// runs first.
    ///
    /// Multi-unit like [`Self::mls_reseal`], so `Resealed { owed: > 0 }` is the
    /// ordinary partly-owed reading — progress and unfinished at once.
    pub drafts_reseal: Option<fauna_client_drafts::DraftsResealProgress>,
}

/// Per-page `error-message` text, guarding the one entry that can never be
/// silently clobbered: a stolen-identity succession's persist-failure message
/// (`settings.md` § Recovery kit → *The persist-failure message survives the
/// page*, ratified 2026-09-14). That message is the ONLY surviving copy of
/// the account's new identity secret, and it renders on the same shared,
/// many-writer `Page::Settings` slot every other Settings control writes to —
/// so once [`Self::park_stolen_failed_message`] parks one, every ordinary
/// `insert`/`remove` targeting `Page::Settings` is a no-op until
/// [`Self::acknowledge_stolen_failed_message`] discharges it. The method names
/// deliberately mirror `HashMap`'s (`insert`/`remove`/`get`/`contains_key`/
/// `is_empty`/`clear`) so every existing call site — roughly 126 of them
/// across `settings/mod.rs`, `settings/account.rs`, `app.rs` and
/// `automation.rs` — keeps compiling unchanged and gets the guard for free,
/// rather than needing to be individually routed through it.
#[derive(Debug, Default)]
pub struct PageErrors {
    map: HashMap<Page, String>,
    /// Set only by [`Self::park_stolen_failed_message`]; cleared only by
    /// [`Self::acknowledge_stolen_failed_message`] or [`Self::clear`] (a full
    /// account-switch teardown, which drops the whole authenticated surface
    /// the message belongs to anyway).
    stolen_failed_pending: bool,
}

impl PageErrors {
    fn new() -> Self {
        Self::default()
    }

    /// Ordinary write — what every non-guarded caller already does. A no-op
    /// for `Page::Settings` while a persist-failure message is pending.
    ///
    /// Takes `String`, not `impl Into<String>`: matching `HashMap::insert`'s
    /// own signature exactly (rather than a nicer-looking generic one) is
    /// what keeps every existing `app.errors.insert(page, "literal".into())`
    /// call site's own `.into()` inferring the same way it always did — a
    /// generic bound here left several of them "type annotations needed"
    /// ambiguous instead.
    pub fn insert(&mut self, page: Page, message: String) -> Option<String> {
        if page == Page::Settings && self.stolen_failed_pending {
            return self.map.get(&page).cloned();
        }
        self.map.insert(page, message)
    }

    /// Ordinary clear — what every non-guarded caller already does. A no-op
    /// for `Page::Settings` while a persist-failure message is pending, same
    /// guard as [`Self::insert`].
    pub fn remove(&mut self, page: &Page) -> Option<String> {
        if *page == Page::Settings && self.stolen_failed_pending {
            return self.map.get(page).cloned();
        }
        self.map.remove(page)
    }

    pub fn get(&self, page: &Page) -> Option<&String> {
        self.map.get(page)
    }

    // Only ever asserted on, crate-wide — never read by production code.
    #[cfg(test)]
    pub fn contains_key(&self, page: &Page) -> bool {
        self.map.contains_key(page)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Full teardown (account switch / sign-out): drops everything,
    /// including a still-pending persist-failure message — the whole
    /// authenticated surface it belongs to is gone with it.
    pub fn clear(&mut self) {
        self.map.clear();
        self.stolen_failed_pending = false;
    }

    /// Park a stolen-identity ceremony's persist-failure message — the ONE
    /// write allowed to land while a previous one is still pending (there is
    /// nothing else it could mean: a ceremony cannot start while the account
    /// is signed in as the identity that owns the last unacknowledged one).
    /// Bypasses the guard by construction: this IS the message the guard
    /// exists to protect, so it cannot go through the ordinary [`Self::insert`].
    pub fn park_stolen_failed_message(&mut self, message: String) {
        self.map.insert(Page::Settings, message);
        self.stolen_failed_pending = true;
    }

    /// Discharge a pending persist-failure message on the nav edge away from
    /// the Account sub-page (`App::sync_recovery_message_nav_edge`): lifts
    /// the guard AND deletes the entry — a bare guard-lift would leave the
    /// stale message to reappear the next time `Page::Settings` is read
    /// (Root, another sub-page, or Account itself on a later visit).  A
    /// no-op when nothing is pending.
    /// Whether a persist-failure message is parked and not yet acknowledged.
    pub fn stolen_failed_pending(&self) -> bool {
        self.stolen_failed_pending
    }

    pub fn acknowledge_stolen_failed_message(&mut self) {
        if self.stolen_failed_pending {
            self.stolen_failed_pending = false;
            self.map.remove(&Page::Settings);
        }
    }
}

/// A continuation waiting for every in-flight teardown stop to finish
/// ([`App::after_stops`]).
pub(crate) type AfterStop = Box<dyn FnOnce(&mut App)>;

/// Whole-app state painted by `ui::render`.
pub struct App {
    /// Current page = the selected sidebar row (selection is navigation,
    /// like the linux `gtk::ListBox` sidebar). While the focus ring is in
    /// [`Zone::Sidebar`] this **is** the ring's position — one piece of state,
    /// so a highlighted row can never drift from the painted page.
    pub page: Page,
    /// Which pane the keyboard drives (authenticated only). Starts on the
    /// sidebar: at login the page pane may hold nothing focusable yet, and a
    /// user who lands with no visible focus has no way to discover ←/→.
    pub zone: Zone,
    /// The authenticated session, `None` pre-login. Gates the whole nav
    /// shell (ui.yaml `navigation.hidden_when: unauthenticated`).
    pub session: Option<crate::session::Session>,
    /// Live state of the nest WS-RPC connection, rendered by the global
    /// `connection-status` element. Driven by the session's supervisor
    /// watch pump; honest `Disconnected` while signed out.
    pub connection: ConnectionState,
    /// Every connection-state report the indicator has taken, and how many
    /// changed its word — the stickiness observable behind
    /// `fauna_e2e_agent::CONNECTION_REPORTS_KEY`. Fed only by the
    /// `ConnectionState` message, i.e. by exactly what the indicator paints from.
    pub connection_reports: fauna_e2e_agent::ConnectionReports,
    /// Every error surface a painted frame has shown this process — the
    /// "no error anywhere" observable behind `fauna_e2e_agent::PAINTED_ERRORS_KEY`.
    /// Fed by the render loop from each frame's registry, which only exists
    /// when the e2e agent does, so a production build never touches it.
    pub painted_errors: fauna_e2e_agent::PaintedErrorTally,
    /// Per-page `error-message` text (ui.yaml: every page has one).
    pub errors: PageErrors,
    /// Whether the last nav-edge check ([`Self::sync_recovery_message_nav_edge`])
    /// found the user on Settings → Account. Purely internal bookkeeping for
    /// that edge-trigger; nothing else reads it.
    was_on_settings_account: bool,
    /// A stolen-identity ceremony this device started is still running — set
    /// when `identity-stolen-button` dispatches the ceremony, cleared by its
    /// `RecoverySucceeded` fold. While it is set, a mid-session supersession is
    /// this device's own doing and the fold owns what happens next
    /// ([`Self::defer_own_supersession`]).
    pub(crate) stolen_ceremony_in_flight: bool,
    /// The ceremony ended WITHOUT adopting a successor while the user was on
    /// Settings → Account, so its message (typically the undecidable arm's
    /// *the outcome is unknown — reopen the app*) is what the screen shows. Set
    /// by the `RecoverySucceeded` fold, cleared on the nav edge away from
    /// Account and by every teardown.
    ///
    /// It extends [`Self::owns_its_supersession`] past the fold, because the
    /// refusal the ceremony causes has no fixed order against the fold: the
    /// live launch machine's `superseded` phase was measured landing ~30 ms
    /// AFTER an undecidable fold, and routing it then replaced the message the
    /// user needs to get back in with an import screen for a key they were
    /// never shown.
    pub(crate) stolen_outcome_on_screen: bool,
    /// A mid-session supersession that arrived while this device's own
    /// ceremony (or its persist-failure message) still owned the screen, and
    /// whose escalation to the launch surface is therefore owed once that
    /// flow is done ([`Self::escalate_deferred_supersession`]).
    superseded_deferred: bool,
    /// Test-agent injected transient messages — the cross-app `messages`
    /// state patch (`set_state({"messages": {"error"|"warning"|"info": …}})`),
    /// the tui twin of linux's test-agent banner labels. The error rides the
    /// shared `error-message` line (an injected error wins while set —
    /// [`Self::error_line_text`]); warning/info paint their own global
    /// `warning-message`/`info-message` lines. All three clear on any nav
    /// patch, mirroring linux ("navigating away dismisses transient messages").
    pub injected_error: Option<String>,
    /// What the last sign-out's erase could not remove, while it still owes
    /// work — the `sign-out-residue` view `identity_choice` paints
    /// (`account-scoping.md` § Erasure follows scope → *the residue surface*).
    /// Set by the sign-out and by a signed-out launch whose silent re-sweep
    /// left something; replaced by every Remove Again; cleared when a session
    /// is established. The durable copy is the install-scoped record
    /// (`fauna_client_accounts::SignOutResidue`) — this is its paint.
    pub sign_out_residue: Option<crate::account_scope::ResidueSurface>,
    pub injected_warning: Option<String>,
    pub injected_info: Option<String>,
    /// The post-succession group sweep's outcome — the ceremony's own result,
    /// rendered in the flow that ran it (`identity-succession.md` § Propagation
    /// → *MLS groups*: the unattested roster is the ceremony's outcome, not a
    /// standing alarm).
    ///
    /// ⚠ **Deliberately the one thing [`Self::drop_authenticated_state`] does
    /// NOT drop.** Everything else there is per-identity state that must never
    /// outlive the identity that fetched it — but this is the opposite case: it
    /// describes the identity's *departure*, and the account switch is the
    /// ceremony's own closing act. Dropping it at the switch would delete the
    /// ceremony's result at the exact moment the ceremony completed, leaving
    /// the user no way to learn which groups were re-pointed or which members
    /// the sweep could not vouch for. Cleared when the user acknowledges it, and
    /// by a full [`Self::reset`].
    pub succession_sweep: Option<crate::settings::SweepStatus>,
    /// The user pressed *Review The Rest Later* on the ephemeral kit-side
    /// review pass, so it stops rendering for the rest of this run
    /// (`identity-succession.md` § Propagation → *MLS groups*, ruling 1).
    ///
    /// **Deferring hides the pass; it decides nothing.** Every item stays open
    /// exactly as it was — which is the whole mechanism by which the permanent
    /// view "inherits only what was still unhandled when the kit was closed or
    /// deferred". A verdict written here would be the opposite of what the
    /// button says.
    ///
    /// Lifetime mirrors [`Self::succession_sweep`] deliberately, because it is a
    /// decision *about* that sweep: it survives the account switch (the pass
    /// renders in the successor's session, and a defer taken there must stick),
    /// and only a full [`Self::reset`] clears it. Session-scoped by design —
    /// a relaunch is a new chance to work the backlog, and the permanent view is
    /// what holds it in between.
    pub succession_review_deferred: bool,
    /// The post-succession **aftermath**, leg by leg, as a progress surface
    /// (`succession-aftermath.md` § Re-key scope: "started at first successor
    /// sign-in, **surfaced with progress**, resumed until complete").
    ///
    /// Every field `None` on an ordinary session — the pass only runs for an
    /// identity the registry knows has predecessors, so a user who never
    /// succeeded pays one in-memory walk and renders nothing.
    ///
    /// **Session-scoped, unlike [`Self::succession_sweep`].** The sweep records
    /// a ceremony that happened once; this records the state of passes that
    /// re-run at *every* sign-in until they report done, so carrying a stale
    /// verdict across a sign-out would render last session's answer over this
    /// session's live one. Cleared with the rest of the authenticated state.
    pub aftermath: AftermathProgress,
    /// The open **unattested-member reviews** this identity carries — one entry
    /// per person a succession's group sweep could not vouch for
    /// (`identity-succession.md` § Propagation → *MLS groups*).
    ///
    /// The cached read behind two renderings: the mark + Keep pair on each
    /// `thread-member-chip`, and the badge on a contact row. It is a cache on
    /// purpose — a member list paints on every frame while the ledger changes
    /// only when a ceremony raises items or the owner answers one — and the
    /// per-row question is asked through the shared
    /// [`fauna_core::data::is_under_review`], never an inline scan, so the join
    /// cannot silently stop matching in one renderer and not the other.
    ///
    /// ⚠ **Refreshed at the post-auth hook and after each adjudication, not per
    /// paint.** The window that leaves open is a *peer device's* verdict, which
    /// shows here until the next read — i.e. this app can re-ask a question
    /// another device already answered. That is the deliberate direction:
    /// § Propagation rules a re-asked question harmless and a silently hidden
    /// flagged person the failure the surface exists to prevent, and
    /// [`fauna_client_config::decide_member_review`] answers a stale press with
    /// a no-op rather than an error.
    ///
    /// Per-identity, so [`Self::drop_authenticated_state`] drops it — unlike
    /// [`Self::succession_sweep`], which describes the *departing* identity.
    pub member_reviews: Vec<fauna_core::data::MemberReview>,
    /// **Email filter rules a succession carried across that the owner has yet
    /// to keep or remove** — the filter plane's twin of [`Self::member_reviews`]
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across, the fourth plane, 2026-08-14). Ids only: the rule itself lives
    /// nest-side and the filter list already holds it.
    ///
    /// A cache for the same reason and refreshed at the same two moments (the
    /// post-auth hook and after each adjudication), through the same
    /// [`Self::ledger_store`] seam — one succession-ledger shape for this
    /// app, not two. The same deliberate staleness direction applies: a peer
    /// device's verdict shows here until the next read, so this app can re-ask
    /// an answered question, which the ruling calls harmless where hiding a
    /// flagged rule is not.
    ///
    /// Per-identity, so [`Self::drop_authenticated_state`] drops it.
    pub filter_marks: Vec<i64>,
    /// The succession-ledger seam [`Self::member_reviews`] and
    /// [`Self::filter_marks`] are read and written through
    /// (`fauna.state.succession-ledger`) — the account-store handle, set at the
    /// `AccountStoreReady` edge and `None` before it, where the post-store-ready
    /// pass (`session::spawn_ledger_aftermath`) re-reads both surfaces the
    /// moment it lands. Drops with the rest of the authenticated state.
    pub ledger_store: Option<Arc<dyn fauna_client_config::SuccessionLedgerStore>>,
    /// The successor still owes itself a recovery kit — set by the succession
    /// fold the instant the ceremony lands, consumed by the *successor's*
    /// post-auth hook, which mints and displays it as the ceremony's closing
    /// act (`identity-succession.md` § The RecoveryKey → *At succession*).
    ///
    /// ⚠ **A second field [`Self::drop_authenticated_state`] deliberately does
    /// NOT drop**, and for the same reason as [`Self::succession_sweep`]: the
    /// obligation is created by the *outgoing* identity's ceremony and can only
    /// be discharged by the *incoming* one, so the switch that separates them is
    /// precisely the boundary it has to cross. Dropping it there would silently
    /// delete the ceremony's last step.
    ///
    /// **Why it exists at all, rather than leaving the successor's
    /// `NeverCreated` warning to prompt them.** The succession transaction
    /// deletes the old `recovery_escrow` row and the old kit retires with the
    /// old identity, so between the ceremony landing and this mint the account
    /// has **no recovery kit and no escrow** — a second seed loss is
    /// unrecoverable, for a user who has just proven they are a theft target.
    /// A background mint cannot close it either: the secret may never be
    /// persisted (§ The RecoveryKey → *Custody*), so an unshown mint would
    /// register a kit **nobody holds**, leaving the user strictly worse off than
    /// never-created — their only route back to a held kit would be the 30-day
    /// seed-alone replacement, which would also fire the pending-replacement
    /// critical alert on their own remediation. Mint **and show**, therefore, or
    /// not at all.
    pub succession_kit_owed: bool,
    /// The 64-hex actor id of the identity the succession just retired — set
    /// beside [`Self::succession_kit_owed`] and crossing the same switch, for
    /// the same reason.
    ///
    /// It is what the owed kit ceremony seals into the escrow blob's
    /// predecessor section: post-succession the predecessor seed exists **only**
    /// in this device's account registry while the corpus is still sealed under
    /// it, so until the re-seal completes a total device loss would restore the
    /// account and leave that corpus unopenable forever
    /// (`succession-aftermath.md` § Re-key scope — the ratified device-loss
    /// race). Carrying the id rather than the seed is deliberate: the registry
    /// stays the secret's one home, and a missing row is then an honest answer
    /// rather than a stale copy.
    pub succession_predecessor: Option<String>,
    /// A test-agent command the app **refused** — convention 11's loud failure,
    /// written only by [`Self::report_refused_agent_command`] (see there for why
    /// this is neither [`Self::injected_error`] nor the per-page [`Self::errors`]
    /// map). Its lifetime is deliberately **one test**: `reset()` clears it, and
    /// `reset()` is what every `app` fixture calls before a test body runs, so a
    /// refusal can neither be wiped early by navigation nor leak into the next
    /// test of a session-scoped app process.
    pub refused_agent_command: Option<String>,
    /// The last [`UiMessage::BarrierProbe`] token this app applied — the only
    /// observable the `barrier` self-test reads. Same one-test lifetime as
    /// [`Self::refused_agent_command`] above: `reset()` clears it, so a token
    /// cannot leak into the next test of a session-scoped app process.
    pub barrier_probe: Option<String>,
    /// What [`Self::barrier_probe`] held at the moment the last `barrier`
    /// acked — written once per barrier, never recomputed
    /// ([`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`], which documents why the live
    /// field above is NOT assertable). Same one-test lifetime.
    pub barrier_ack_probe: Option<String>,
    /// How many times this app has torn the authenticated session down —
    /// [`fauna_e2e_agent::SESSION_GENERATION_KEY`], the observable convention
    /// 14's negative asserts read to prove a gesture did NOT relaunch them.
    ///
    /// Bumped by [`Self::drop_authenticated_state`], which is the ONE teardown
    /// every arm shares (a full [`Self::reset`], an account switch, a sign-out),
    /// so tui gets the whole contract from a single increment and no arm can be
    /// added later that forgets to count itself. It is also genuinely the
    /// *initiation* point the contract asks for: tui tears down inline in the
    /// handler rather than deferring, so the bump is already synchronous with
    /// the gesture that caused it.
    ///
    /// Deliberately NOT reset per test, unlike the two probe fields above: a
    /// `reset()` is itself a teardown, so zeroing it there would erase the event
    /// the counter exists to report. Tests read a delta across their own
    /// gesture, so accumulation costs them nothing — and this is what keeps
    /// linux's and web's counters (which cannot clear, being process- and
    /// storage-backed) the SAME observable rather than three per-app dialects.
    pub session_generation: u64,
    /// The onboarding wizard — a paint shell over the shared machine. Painted
    /// whenever `session` is `None` **and** the launch surface has handed the
    /// screen over; its machine outlives sign-out so a re-onboard starts clean
    /// via `Wizard::reset`.
    pub wizard: crate::wizard::Wizard,
    /// Which unauthenticated surface owns the screen: the launch flow's own
    /// (`launch_retry`, the spinner) or the wizard. `onboarding.md`
    /// § App-launch routing.
    pub launch: crate::launch::LaunchSurface,
    /// Append-mode "Add account" is running the onboarding wizard OVER a live
    /// session (`long-term-store.md` § Multi-account evolution). When set, the
    /// screen shows the wizard surface even though [`Self::session`] is `Some` —
    /// the immediate-mode analog of linux's separate append window (whose live
    /// session is never torn down, so an abandon is a pure return). Set by
    /// [`Self::enter_add_account`], cleared by [`Self::abandon_add_account`], the
    /// append switch, or any teardown ([`Self::drop_authenticated_state`]).
    pub adding_account: bool,
    /// The feed page. Its `FeedManager` is built at the post-auth hook and
    /// dropped on sign-out, so its lifetime is the session's — which is why it
    /// hangs off `App` rather than being a process-wide `OnceLock` singleton.
    pub feed: crate::feed::FeedState,
    /// The conversations page. Its `ConversationsManager` is built at the same
    /// post-auth hook and dropped on sign-out, exactly like [`Self::feed`]
    /// (linux's `OnceLock` singleton, but session-scoped on `App` here).
    pub conversations: crate::conversations::ConversationsState,
    /// The Media page. Its `MediaMachine` is built at the same post-auth hook
    /// and dropped on sign-out, like [`Self::feed`] — and it carries the
    /// per-actor owner `BackupKey` an upload seals under, so it must not
    /// outlive the session either.
    pub media: crate::media::MediaState,
    /// The A6 sync-agent control surface (`crate::sync_agent`). Provisions the
    /// external per-user `fauna-sync-agent` at the post-auth hook and drives its
    /// folder-binding seam; session-scoped like [`Self::media`] — built at that
    /// hook and **unprovisioned** on sign-out/switch/reset (never on a plain
    /// quit). Also carries the `sync-agent-status` health reading the shell
    /// paints. A no-op empty on non-unix (under e2e it really provisions —
    /// the spawner just direct-child-spawns the agent, A6 Slice 4).
    pub sync_agent: crate::sync_agent::SyncAgentState,
    /// The contacts page's state (roster + knocks + the Find User form) —
    /// transport-only, no shared manager (`contacts.md` § Where logic lives),
    /// so the rows live here and refetch at login + on nav-to-tab.
    pub contacts: crate::contacts::ContactsState,
    /// The standalone Nostr page (`ui/nostr.md` § Page structure — its own
    /// page, like mail). Transport-only over the shared typed `BridgesClient`
    /// (no manager owns a snapshot), so the bridge status lives here and
    /// refetches on every nav to the tab.
    pub nostr: crate::nostr::NostrState,
    /// The unified feed-side Bridges page (`behavior/bridges.md`) — the multi-
    /// bridge twin of [`Self::nostr`], transport-only over the same shared typed
    /// `BridgesClient`, so the filtered bridge rows live here and refetch on
    /// every nav to the tab.
    pub bridges: crate::bridges::BridgesState,
    /// The Backups page's destination surface (`ui/backups.md` § Manage backup
    /// destinations) — the configured destinations plus the nest's
    /// `fauna.backup.status` projection for them, re-read on every nav to the
    /// tab. Transport-only over the shared `fauna-client-config` calls; no
    /// manager owns a snapshot.
    pub backups: crate::backups::BackupsState,
    /// The notifications page's state (rows + the nest-reported unread
    /// count) — transport-only like contacts (`behavior/notifications.md`
    /// § State & data shape), refetched at login + on nav-to-tab.
    pub notifications: crate::notifications::NotificationsState,
    /// The Moderation page's state — the merged queue only
    /// (`behavior/moderation.md` § Layout & flow). Its two sources live
    /// elsewhere: the server half is fetched per visit, the local half is read
    /// from the conversations session's shared `LocalDetectionStore`.
    pub moderation: crate::moderation::ModerationState,
    /// The shared report sheet (`crate::report`) — app-wide, because the feed,
    /// conversations and profile pages all open the one component.
    pub report: crate::report::ReportState,
    /// The profile page's state (the viewed actor + the last identity / offers /
    /// block fetch). No observer manager (`ui/profile.md` § Persistence): the page
    /// re-fetches on every open (`Self::open_profile`).
    pub profile: crate::profile::ProfileState,
    /// The events (calendar) page's state — transport-only over
    /// `fauna_client_caldav::CalDavClient` (no shared manager), refetched at
    /// login + on nav-to-tab like [`Self::notifications`].
    pub events: crate::events::EventsState,
    /// The Settings shell's state (which sub-page shows + the client-local prefs).
    /// Unlike the session-scoped pages above, this is **not** session-dependent —
    /// the external-media preference is client-local (`crate::settings`), so it is
    /// hydrated once at construction and outlives sign-out.
    pub settings: crate::settings::SettingsState,
    /// The global search page's state — transport-only over
    /// `fauna_client_search::SearchClient` (no shared manager), built at the
    /// post-auth hook like [`Self::notifications`]. No nav-edge refetch (there
    /// is no query until the user submits one).
    pub search: crate::search::SearchState,
    /// Whether the signed-in actor is a nest admin — the shared
    /// `fauna.account.am_i_admin` gate (`admin.md` § Shell), computed once in
    /// shared Rust and only *rendered* here (never recomputed per-app).
    /// **Fail-closed:** `false` until the post-auth check lands
    /// ([`crate::admin::init`]) and on any error, so the gated `admin-tab`
    /// sidebar row is never shown to a non-admin ([`Self::sidebar_pages`]). Reset
    /// on sign-out/reset so a re-auth as a non-admin can't inherit a stale `true`.
    pub am_i_admin: bool,
    /// The nest-admin shell's state (`crate::admin`) — the live client + the
    /// loaded dashboard stats. Session-scoped like the other managers: built at
    /// the post-auth hook, dropped on sign-out.
    pub admin: crate::admin::AdminState,
    /// Whether the signed-in actor is in ANY family relationship — supervised,
    /// guarding ≥1 ward, or named on an incoming transfer proposal
    /// (`family-safety.md` § App surface / § Graduation & transfer →
    /// Visibility). The gated `family-tab` twin of [`Self::am_i_admin`], and
    /// **fail-closed** the same way: `false` until the post-auth
    /// `fauna.family.status` read lands ([`crate::family::spawn_status_check`])
    /// and on any error, so the row is never shown to an unrelated account.
    /// Reset on sign-out/switch so a re-auth cannot inherit a stale `true`.
    pub has_family: bool,
    /// The gated Family page's state (`crate::family`) — the one status read
    /// both roles render from, plus the guardian's queue and editor buffer.
    /// Session-scoped like [`Self::admin`].
    pub family: crate::family::FamilyState,
    /// The ward's screen-time lock + usage heartbeat (`family-safety.md`
    /// § Screen time, Slice E). Fed by every `fauna.family.status` read and
    /// re-asked on the one-minute tick; session-scoped like [`Self::family`],
    /// since a stale policy must never lock the *next* identity.
    pub screen_lock: crate::screen_lock::ScreenLock,
    /// The viewer's content render-enforcement state — their own spam/phishing
    /// thresholds, any guardian content floor, and the Guardian Notify counter
    /// (`family-safety.md` § Content policy, § Guardian Notify). Read by both
    /// social surfaces on every paint; session-scoped like [`Self::family`],
    /// since a stale floor must never gate the *next* identity's feed.
    pub content_policy: crate::content_policy::ContentPolicyState,
    /// The region content plane (`region-blocking.md` § The content plane) —
    /// the declared region, the device's last-known-good record and its relay
    /// refresh. **Device-scoped, not session-scoped**: restored at start ahead
    /// of the first fetch, kept across identity changes (a region is a fact
    /// about the device), and re-applied to [`Self::content_policy`] whenever
    /// that is reset.
    pub region: crate::region::RegionState,
    /// The headless-store unlock/create surface's buffers + error
    /// (`crate::unlock`; painted when [`Self::launch`] is `Unlock` /
    /// `CreatePassphrase`).
    pub unlock: crate::unlock::UnlockState,
    /// The locked launch surface's `identity_stolen_entry` buffers and outcome
    /// (`crate::locked`).
    pub locked: crate::locked::LockedState,
    /// The shared launch machine, alive for the process: `Retry` re-runs its
    /// silent challenge, and once `Online` it is the session's bearer source.
    pub launch_machine: Option<std::sync::Arc<fauna_launch_machine::LaunchMachine>>,
    /// The long-term credential store, resolved once at construction and owned
    /// for the process. Everything that reads or writes an account goes through
    /// this handle rather than reconstructing one from the environment, so the
    /// store a unit test sees is the store the code under test uses — which is
    /// what makes [`Self::reset`]'s namespace wipe safe to unit-test at all
    /// (`test_app` injects a file backend; a rebuilt-from-env store would
    /// default to the developer's real `fauna-tui` keyring namespace).
    pub credentials: Arc<CredentialStore>,
    /// The backend→UI sender, so a message handler can start work of its own
    /// (the launch router establishing a session, a retry re-arming the machine)
    /// without threading `tx` through every call site.
    pub tx: tokio::sync::mpsc::UnboundedSender<UiMessage>,
    /// The teardown stops `session::sign_out` has spawned and not yet seen
    /// finish, and the work queued behind them ([`Self::after_stops`]) — the
    /// shared ordering (`fauna_client_account_runtime::StopQueue`, which linux
    /// drives too). `App`-owned like everything else here: the loop that owns
    /// `App` is the only thing that touches it.
    pub(crate) stops: fauna_client_account_runtime::StopQueue<AfterStop>,
    /// The app-wide critical-alerts registry (`behavior/critical-alerts.md`):
    /// feeders post keyed possible-compromise / data-loss-imminent conditions,
    /// [`crate::ui::render_shell`] paints them as the every-page banner. Owned
    /// per-`App` rather than in a process-wide static (linux's shape) because
    /// tui's unit tests share one process — see
    /// [`crate::critical_alerts`]. Lifetime is per app session:
    /// [`Self::drop_authenticated_state`] clears it.
    pub alerts: Arc<fauna_client_alerts::CriticalAlerts>,
    /// The current identity's handle on its re-sweep loop's wait — empty in a
    /// production build; the e2e agent's `alert_sweep_wake` fires it
    /// ([`crate::critical_alerts::SweepWake`]). Replaced at every session
    /// establishment, never shared across identities.
    pub alert_sweep_wake: crate::critical_alerts::SweepWake,
    /// The unauthenticated screen's focus ring, indexed over the *focusable*
    /// elements of [`Self::screen_elements`]. It lives here, not on `Wizard`,
    /// because the launch surface has focusable CTAs of its own.
    pub focus: usize,
    /// The wizard step as of the last observer tick — the edge detector behind
    /// the recovery pages' **page-entry** reads (see [`Self::on_wizard_changed`]).
    /// The machine notifies on *every* mutation, so firing a network read per
    /// tick would hammer the nest; firing on the step *edge* fires once a visit.
    last_wizard_step: Option<fauna_onboarding_machine::OnboardingStep>,
    /// A `fauna://` route waiting for a signed-in session — the launch
    /// argument (`crate::routes`), or one the e2e seam fed in while signed
    /// out. Applied once through [`Self::apply_route`] by
    /// [`Self::apply_pending_route`], never during the launch flow.
    pub pending_route: Option<fauna_core::app_route::AppRoute>,
    pub should_quit: bool,
}

/// The async work a field write implies, across pages — the single type
/// [`App::set_field`] hands back so the two callers (the agent's type path,
/// which **awaits** it because element reads are single-shot; the keyboard's,
/// which **spawns** it so a slow nest never freezes the render loop) need only
/// one `run()`. Each variant owns only `Arc`s, so it crosses a `tokio::spawn`.
pub enum PendingWrite {
    /// A feed search re-query (`feed.md` § Anti-patterns: the client never
    /// filters the loaded list locally).
    FeedSearch(crate::feed::PendingSearch),
    /// A conversations recipient-resolve (the backend probe that settles the
    /// picker's terminal `state` and promotes a typed address to its real rail).
    ConvResolve(crate::conversations::PendingResolve),
}

impl PendingWrite {
    pub async fn run(self) {
        match self {
            PendingWrite::FeedSearch(p) => p.run().await,
            PendingWrite::ConvResolve(p) => p.run().await,
        }
    }
}

impl App {
    /// The production constructor: the credential store comes from the
    /// environment (`FAUNA_KEYRING_APP` / `FAUNA_E2E_CREDENTIAL_DIR`).
    pub fn new(tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>) -> Self {
        let mut app = Self::with_credentials(tx, Arc::new(crate::session::secret_store()));
        // Hydrate the client-local tui prefs from the real config dir. The seam
        // constructor above leaves an empty (no-disk) default so unit tests never
        // read/write the developer's real `prefs.json`; only production loads it.
        app.settings = crate::settings::SettingsState::load(crate::session::config_dir());
        // The region plane's device record, restored ahead of the first relay
        // fetch (`region-blocking.md` § Fail posture) — before any session, so
        // the very first feed paint already folds a last-known-good policy.
        app.region = crate::region::RegionState::launch();
        crate::region::apply_rule_sets(&app);
        app
    }

    /// [`Self::new`] over an explicit credential store — the seam unit tests use
    /// to stay off a real keyring.
    pub fn with_credentials(
        tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>,
        credentials: Arc<CredentialStore>,
    ) -> Self {
        App {
            page: Page::ALL[0],
            zone: Zone::Sidebar,
            session: None,
            connection: ConnectionState::Disconnected,
            connection_reports: fauna_e2e_agent::ConnectionReports::default(),
            painted_errors: fauna_e2e_agent::PaintedErrorTally::default(),
            errors: PageErrors::new(),
            was_on_settings_account: false,
            stolen_ceremony_in_flight: false,
            stolen_outcome_on_screen: false,
            superseded_deferred: false,
            injected_error: None,
            sign_out_residue: None,
            injected_warning: None,
            injected_info: None,
            succession_sweep: None,
            succession_review_deferred: false,
            succession_kit_owed: false,
            succession_predecessor: None,
            aftermath: AftermathProgress::default(),
            member_reviews: Vec::new(),
            filter_marks: Vec::new(),
            ledger_store: None,
            refused_agent_command: None,
            barrier_probe: None,
            barrier_ack_probe: None,
            session_generation: 0,
            wizard: crate::wizard::Wizard::new(tx, &credentials),
            launch: crate::launch::LaunchSurface::Wizard,
            adding_account: false,
            feed: crate::feed::FeedState::default(),
            conversations: crate::conversations::ConversationsState::default(),
            media: crate::media::MediaState::default(),
            sync_agent: crate::sync_agent::SyncAgentState::default(),
            contacts: crate::contacts::ContactsState::default(),
            nostr: crate::nostr::NostrState::default(),
            bridges: crate::bridges::BridgesState::default(),
            backups: crate::backups::BackupsState::default(),
            notifications: crate::notifications::NotificationsState::default(),
            moderation: crate::moderation::ModerationState::default(),
            report: crate::report::ReportState::default(),
            profile: crate::profile::ProfileState::default(),
            events: crate::events::EventsState::default(),
            settings: crate::settings::SettingsState::default(),
            search: crate::search::SearchState::default(),
            am_i_admin: false,
            admin: crate::admin::AdminState::default(),
            has_family: false,
            family: crate::family::FamilyState::default(),
            screen_lock: crate::screen_lock::ScreenLock::default(),
            content_policy: crate::content_policy::ContentPolicyState::default(),
            region: crate::region::RegionState::empty(),
            unlock: crate::unlock::UnlockState::default(),
            locked: crate::locked::LockedState::default(),
            launch_machine: None,
            credentials,
            tx: tx.clone(),
            stops: fauna_client_account_runtime::StopQueue::default(),
            alerts: crate::critical_alerts::registry(tx.clone()),
            alert_sweep_wake: crate::critical_alerts::SweepWake::default(),
            focus: 0,
            last_wizard_step: None,
            pending_route: None,
            should_quit: false,
        }
    }

    pub fn authenticated(&self) -> bool {
        self.session.is_some()
    }

    /// **The one door a `fauna://` route takes** (`apps/tui.md` § System
    /// integration → *In-app routes*) — the launch argument and the e2e
    /// `open_route` command alike. Navigation only (`fauna_core::app_route`):
    /// it reveals a surface, never answers it.
    ///
    /// Signed out (or with the launch flow on screen) the route is held in
    /// [`Self::pending_route`] and `None` returned; [`Self::apply_pending_route`]
    /// brings it back here once the session is signed in. Returns the page
    /// work the route implies, so the caller keeps the await-vs-spawn choice
    /// (the [`Self::apply`] duality: the agent awaits, the loop spawns).
    #[must_use = "the route's page work must be awaited (agent) or spawned (loop), or the card never paints"]
    pub fn apply_route(&mut self, route: fauna_core::app_route::AppRoute) -> Option<PageOp> {
        use fauna_core::app_route::AppRoute;
        if self.showing_launch_surface() {
            self.pending_route = Some(route);
            return None;
        }
        match route {
            AppRoute::Consent { request_uri } => {
                // The connected-apps sub-page's own visit read is replaced by
                // the open, which re-reads the page itself; the Settings
                // landing's nav-edge read is for the Root page this route
                // never shows.
                let _ = self.apply(Page::Settings);
                let _ = crate::settings::route_subpage(
                    &mut self.settings,
                    Some("connected-apps"),
                    None,
                );
                let op =
                    crate::settings::connected_apps_open_handoff_op(&self.settings, request_uri);
                if op.is_none() {
                    tracing::warn!(target: "fauna_tui::routes", "consent route: no connected-apps machine in this session");
                }
                op.map(PageOp::Settings)
            }
            // No producer of the two share routes exists on this platform yet
            // (today only the Windows Explorer leaf mints them); they take
            // this door the day one does.
            AppRoute::ShareLink { .. } | AppRoute::FolderShare { .. } => {
                tracing::warn!(target: "fauna_tui::routes", "dropping a share route: tui has no producer for it");
                None
            }
        }
    }

    /// Apply the held route, once a signed-in session owns the screen. Called
    /// by the main loop before every draw; a no-op while nothing is held or
    /// the launch flow is still up.
    #[must_use = "the route's page work must be spawned, or the card never paints"]
    pub fn apply_pending_route(&mut self) -> Option<PageOp> {
        if self.showing_launch_surface() {
            return None;
        }
        let route = self.pending_route.take()?;
        self.apply_route(route)
    }

    /// Whether the screen shows an unauthenticated surface (the launch flow or the
    /// onboarding wizard) rather than the authenticated pages. True when signed
    /// out — AND when the append-mode "Add account" wizard is running over a live
    /// session ([`Self::adding_account`]), which is the whole point of the flag:
    /// the wizard owns the screen while the session stays live underneath. The
    /// screen projections ([`Self::page_elements`], the sidebar prepend in
    /// [`Self::screen_elements`], [`Self::screen_error_text`]) branch on THIS, not
    /// `authenticated()` — so append mode routes to the wizard without tearing the
    /// session down (`long-term-store.md` § Multi-account evolution).
    pub fn showing_launch_surface(&self) -> bool {
        // The retire page opened from `admin-nest` is hosted by the wizard over
        // the live session, exactly as the append-mode wizard is.
        !self.authenticated() || self.adding_account || self.wizard.retire.is_some()
    }

    /// Enter append-mode "Add account": hand the screen to a FRESH onboarding
    /// wizard (create-or-import from scratch — a reset machine starts at
    /// `IdentityChoice`, NOT a seed of an existing secret) OVER the live session.
    /// The session is NOT torn down — [`Self::adding_account`] just routes the
    /// screen to the wizard; on the wizard's `LoggedIn` outcome
    /// `wizard::handle_wizard_done` adopts + switches to the new identity, and
    /// Escape ([`Self::abandon_add_account`]) returns to the live session.
    pub fn enter_add_account(&mut self) {
        self.adding_account = true;
        self.wizard.reset();
        self.launch = crate::launch::LaunchSurface::Wizard;
    }

    /// Abandon an in-flight append-mode "Add account" and return to the live
    /// session. Because the session was never torn down, this is a pure return —
    /// no re-auth, no routing touched (an append moves the active pointer only at
    /// its terminal; a provisioning run's custody mint leaves an INACTIVE row the
    /// switcher lists, `onboarding.md` § Multi-account — nothing to heal). This is the in-app escape the
    /// crash-safe recoverability corollary requires — without it a mistaken "Add
    /// account" would strand the user in the wizard (`long-term-store.md`
    /// § Multi-account evolution: "no in-wizard escape" is the bug to avoid).
    pub fn abandon_add_account(&mut self) {
        self.adding_account = false;
        self.wizard.reset();
    }

    /// The sidebar's visible page rows, in order: the 12 always-visible pages
    /// ([`Page::ALL`]), then the two gated entries — `admin-tab` when the shared
    /// [`Self::am_i_admin`] gate passes, then `family-tab` when the
    /// `fauna.family.status` gate does ([`Self::has_family`]). This is the tui
    /// twin of linux's `SidebarItem::ALL` + its appended-hidden gated rows, in
    /// linux's own order (Admin, then Family last).
    ///
    /// The focus ring ([`Self::focus_next`]/[`Self::focus_prev`]/[`Self::focused`])
    /// and the paint ([`crate::ui`]'s `render_sidebar`) BOTH index THIS list, not
    /// `Page::ALL` directly — so a gated row (dis)appearing re-seats the ring
    /// cleanly, and the highlight can never point past the end of a shrunk list.
    pub fn sidebar_pages(&self) -> Vec<Page> {
        let mut pages: Vec<Page> = Page::ALL.to_vec();
        if self.am_i_admin {
            pages.push(Page::Admin);
        }
        if self.has_family {
            pages.push(Page::Family);
        }
        // Always last, below every gated row — the one sidebar entry that
        // isn't a navigation target (`Page::Exit`'s doc comment).
        pages.push(Page::Exit);
        pages
    }

    /// The 0-based position of [`Self::page`] among the visible sidebar rows,
    /// or `None` when the current page isn't a sidebar row (it always is while
    /// authenticated). The single "where is the ring" query both the ring
    /// movement and the paint's `ListState` selection resolve through.
    pub fn sidebar_index(&self) -> Option<usize> {
        self.sidebar_pages().iter().position(|p| *p == self.page)
    }

    /// The whole screen's ordered element list — the one source paint, the
    /// automation registry, and the focus ring all read.
    ///
    /// Authenticated: the sidebar's visible `{page}-tab` rows
    /// ([`Self::sidebar_pages`] — the 12 always-on plus the gated `admin-tab`),
    /// then the current page's elements. Unauthenticated: the wizard's or the
    /// launch surface's (exactly one of the two owns the screen).
    ///
    /// **The element list is the registry; the viewport clips paint only.** A
    /// page lists every element its snapshot implies — all N posts of a feed,
    /// not the handful that fit the terminal — because `count("post-card")` is
    /// answered from the registry this builds. Clipping here would silently cap
    /// every cross-app count assertion at terminal height.
    ///
    /// The render loop registers these same two halves separately —
    /// [`Self::sidebar_elements`], then the page exactly as the frame drew it
    /// (`crate::ui::register_frame`) — so this whole-screen list is the
    /// tests' view of the registry.
    #[cfg(test)]
    pub fn screen_elements(&self) -> Vec<crate::element::Element> {
        let mut elements = self.sidebar_elements();
        elements.extend(self.page_elements());
        elements
    }

    /// The sidebar half of [`Self::screen_elements`] — its `{page}-tab` rows.
    pub fn sidebar_elements(&self) -> Vec<crate::element::Element> {
        // No sidebar while a launch/wizard surface owns the screen — including the
        // append-mode "Add account" wizard over a live session (`adding_account`),
        // which must present as the wizard, not the authenticated shell.
        if self.showing_launch_surface() {
            return Vec::new();
        }
        self.sidebar_pages()
            .into_iter()
            .map(crate::element::Element::tab)
            .collect()
    }

    /// Land the page's focus ring on the page element at `index` — or, when that
    /// element takes no focus itself (a label, a message body), on the first
    /// focusable element after it. `false` when nothing at or after it can take
    /// focus, and the ring is left where it was.
    ///
    /// **In tui the viewport follows the focus ring** (`crate::ui`'s
    /// `scroll_offset`), so this IS the scroll: it is what a user's arrow keys
    /// do to bring something into view, and it is the one door every
    /// programmatic scroll goes through — a search hit's selected message
    /// (`crate::search`) and the automation agent's targeted scroll-into-view
    /// alike. The zone moves to the page too: a page scrolls only while the ring
    /// is in it.
    pub fn focus_page_element(&mut self, index: usize) -> bool {
        let elements = self.page_elements();
        let before = elements
            .iter()
            .take(index)
            .filter(|e| e.focusable())
            .count();
        if before < elements.iter().filter(|e| e.focusable()).count() {
            self.focus = before;
            self.zone = Zone::Page;
            true
        } else {
            false
        }
    }

    /// The current page's own elements — everything but the sidebar.
    ///
    /// This is the focus ring's list while the ring is in [`Zone::Page`], and
    /// the half of [`Self::screen_elements`] a page owns.
    pub fn page_elements(&self) -> Vec<crate::element::Element> {
        let mut elements = self.page_elements_ungated();
        for element in &mut elements {
            self.apply_offline_gate(element);
        }
        elements
    }

    /// The transport state as the lowercase wire word every app family carries
    /// — `fauna_core::format::connection_state_label`'s input, and the offline
    /// gate's. One mapping, two consumers (`crate::ui::connection_status_text`
    /// is the other), so the indicator and the gate can never disagree about
    /// what "connected" means.
    ///
    /// Delegates to `ConnectionState::as_wire_word`, which owns the mapping for
    /// every app family (this used to be tui's own `match`, and the apple leg
    /// would have been the second copy — 2026-08-16).
    pub fn connection_state_word(&self) -> &'static str {
        self.connection.as_wire_word()
    }

    /// Desensitize `element` when the mutation behind it cannot happen without
    /// a live nest — W4 phase 4's UI desensitizing
    /// (`account-data-plane.md` § The offline-mutation contract, class 3).
    ///
    /// **Why here and not at each call site.** This is the same reason the
    /// screen-time lock is applied in [`Self::page_elements`]: gating in the
    /// one place the three consumers read (paint, the automation registry, the
    /// focus ring) makes them agree by construction. It also keeps the rule
    /// where the charter demands it — the decision is the SHARED classification
    /// plus this app's connection state, never a per-app list of
    /// widgets-to-grey. A page author adds no gate code at all; declaring what
    /// their gesture calls (`Action::wire_kind`) is the whole contribution.
    ///
    /// The reason rides [`Element::labelled`] — per affordance, never a global
    /// "you are offline" banner (`account-data-plane.md` § R11) — and only when
    /// the page gave no label of its own, so a page's own hint (the
    /// `folder-webdav-toggle` "set up mail first") is never overwritten by a
    /// weaker one. Disabled elements paint DIM (`crate::ui`), so a human sees
    /// the state and a driver reads it through `is_enabled` / the derived
    /// `disabled` attribute.
    fn apply_offline_gate(&self, element: &mut crate::element::Element) {
        // An already-disabled element keeps the page's own reason: it is
        // unavailable for a stronger, more specific cause than the connection.
        if !element.enabled {
            return;
        }
        let Some(kind) = element.gesture().and_then(|g| g.wire_kind()) else {
            return;
        };
        let verdict = fauna_protocol::offline_class::affordance(kind, self.connection_state_word());
        if verdict.is_available() {
            return;
        }
        element.enabled = false;
        if element.label.is_none()
            && let Some(reason) = verdict.reason()
        {
            element.label = Some(reason.resolve(fauna_i18n::strings::lookup));
        }
    }

    /// The `sign-out-residue` view, on `identity_choice` only and only while a
    /// residue owes work ([`Self::sign_out_residue`]). Never over an
    /// append-mode "Add account" wizard: that user is signed in, and a residue
    /// is only ever reported to a signed-out one.
    fn sign_out_residue_elements(&self) -> Vec<crate::element::Element> {
        let Some(residue) = &self.sign_out_residue else {
            return Vec::new();
        };
        if self.adding_account
            || self.wizard.is_awaiting_manual_dns()
            || self.wizard.machine.step()
                != fauna_onboarding_machine::OnboardingStep::IdentityChoice
        {
            return Vec::new();
        }
        crate::wizard::identity_choice::residue_elements(residue)
    }

    /// [`Self::page_elements`] before the offline gate runs — the page's own
    /// view of its elements. Split out so the gate has exactly one seam to
    /// wrap, and so a test can compare the two.
    fn page_elements_ungated(&self) -> Vec<crate::element::Element> {
        // The launch/wizard surface owns the screen when signed out AND during an
        // append-mode "Add account" over a live session (`adding_account`, whose
        // `launch` is `Wizard`) — so both route here, not to the page match below.
        if self.showing_launch_surface() {
            // The screen-time lock is deliberately NOT consulted here: it is
            // keyed to an authenticated ward, and a launch surface has no
            // identity to lock (the `critical_alert_lines` gate, same reason).
            return match self.launch {
                crate::launch::LaunchSurface::Wizard => {
                    let mut elements = self.wizard.elements();
                    elements.extend(self.sign_out_residue_elements());
                    elements
                }
                crate::launch::LaunchSurface::Unlock => {
                    crate::unlock::elements(self, crate::unlock::Mode::Unlock)
                }
                crate::launch::LaunchSurface::CreatePassphrase => {
                    crate::unlock::elements(self, crate::unlock::Mode::Create)
                }
                crate::launch::LaunchSurface::IdentityStolenEntry { .. } => {
                    crate::locked::entry_elements(self)
                }
                _ => self.launch.elements(),
            };
        }
        // The ward's screen-time lock REPLACES the page pane while it holds
        // (`family-safety.md` § Screen time — "outside the window a conforming
        // ward client renders a full-screen lock"). Gating here rather than in
        // the paint is what makes the three consumers of this list agree by
        // construction: the pane paints the lock, the automation registry
        // registers exactly the lock, and the focus ring cannot Tab into a
        // control that is no longer on screen.
        //
        // The **Family page is exempt** — the goal-doc invariant that a locked
        // ward can always read who supervises them and what the policy says —
        // and the sidebar is untouched (it is `screen_elements`' other half),
        // so `family-tab` and `supervised-indicator` stay live and the exemption
        // is actually reachable.
        if self.page != Page::Family
            && let Some(message) = crate::screen_lock::lock_message(&self.screen_lock)
        {
            return crate::screen_lock::lock_elements(&message);
        }
        let mut elements = self.page_body_elements();
        // The shared report sheet paints flat, once, after the page that opened
        // it — at most one is ever open, so its members read unscoped
        // (`crate::report` module docs).
        elements.extend(crate::report::elements(self));
        elements
    }

    /// The current page's own element list — [`Self::page_elements_ungated`]
    /// appends the report sheet after it.
    fn page_body_elements(&self) -> Vec<crate::element::Element> {
        match self.page {
            Page::Feed => crate::feed::elements(self),
            Page::Conversations => crate::conversations::elements(self),
            Page::Contacts => crate::contacts::elements(self),
            Page::Notifications => crate::notifications::elements(self),
            Page::Moderation => crate::moderation::elements(self),
            Page::Profile => crate::profile::elements(self),
            Page::Events => crate::events::elements(self),
            Page::Search => crate::search::elements(self),
            Page::Settings => crate::settings::elements(self),
            Page::Media => crate::media::elements(self),
            Page::Admin => crate::admin::elements(self),
            Page::Nostr => crate::nostr::elements(self),
            Page::Bridges => crate::bridges::elements(self),
            Page::Family => crate::family::elements(self),
            Page::Backups => crate::backups::elements(self),
            // Empty body — the ring resting here has nothing to preview
            // (`Page::Exit`'s doc comment); actuating it quits instead of
            // "opening" it.
            Page::Exit => Vec::new(),
        }
    }

    /// Text for the screen's `error-message` element, or `None` when the line
    /// must not register — an empty error must read as *invisible*, not as a
    /// blank-but-present element (apple's lesson).
    ///
    /// Authenticated, that is the current page's error. Unauthenticated, the
    /// launch surface's `NeedsUpdate` message renders here (per
    /// `onboarding.md:544`); the transient surface has its own
    /// `launch-transient-error` element instead.
    pub fn screen_error_text(&self) -> Option<String> {
        // A refused test-agent command outranks EVERY other error, on every
        // screen: it means the app never did what the driver asked, so any later
        // product assertion is reading a state the test did not actually set up
        // (`Self::report_refused_agent_command`). Compiled out of release
        // artifacts with the agent itself (convention 15).
        //
        // Checked ABOVE the launch-surface branch, not inside it: a command can
        // be refused while signed out — indeed the honest refusal reason for a
        // session-scoped command (`silent_sign_in`: "no active account") is
        // PRECISELY the pre-auth case — and reporting it only once a page owns
        // the screen would leave the driver reading `error=''` for the one state
        // that can produce it. That is the convention-11 slot bug this app
        // already paid for once (the `injected_error` slot cleared by nav).
        #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
        if let Some(refused) = &self.refused_agent_command {
            return Some(refused.clone());
        }
        // Otherwise: while a launch/wizard surface owns the screen (signed out OR
        // an append-mode "Add account" over a live session), the wizard/launch
        // error renders — not the underlying page's.
        if !self.showing_launch_surface() {
            return self
                .errors
                .get(&self.page)
                .filter(|e| !e.is_empty())
                .cloned()
                .or_else(|| self.page_snapshot_error());
        }
        match self.launch {
            crate::launch::LaunchSurface::Wizard => self.wizard.error_text(),
            crate::launch::LaunchSurface::Unlock
            | crate::launch::LaunchSurface::CreatePassphrase => {
                self.unlock.error.clone().filter(|e| !e.is_empty())
            }
            // Both locked surfaces: the ceremony's outcome stays up across
            // Back (`LockedState::error`).
            crate::launch::LaunchSurface::AccountLocked { .. }
            | crate::launch::LaunchSurface::IdentityStolenEntry { .. } => {
                self.locked.error.clone().filter(|e| !e.is_empty())
            }
            _ => self.launch.error_text(),
        }
    }

    /// The current page's own error, for pages that derive it from a held
    /// snapshot rather than folding it into [`Self::errors`].
    ///
    /// **Why this arm exists.** [`Self::error_line_text`] is meant to be the one
    /// funnel the paint, the registry and the state protocol's `messages.error`
    /// all read, so they cannot disagree. Five pages used to bypass it by
    /// pushing their own `error-message` element from their `elements()` — which
    /// registers the id, and reads correct to a human, but is **invisible to the
    /// cross-app `error_text()`**: that helper reads `messages.error` first and
    /// falls back to the element only when the key is ABSENT, and tui always
    /// emits the key (`crate::automation`'s state payload), so a null there
    /// resolves to `""` and the fallback can never run. The page error was
    /// therefore silently unreadable by every test that asks for it — the
    /// dropped-error-surface shape of `../testing.md` point 2.
    ///
    /// Each page keeps its own precedence chain as a pure `page_error`, because
    /// that logic is the page's (admin-dns puts domain-CRUD feedback ahead of
    /// the list/verify read failure); this only decides *where* it is read.
    /// A page that folds its error into [`Self::errors`] needs no arm here —
    /// the map lookup above already wins.
    fn page_snapshot_error(&self) -> Option<String> {
        match self.page {
            crate::pages::Page::Admin => crate::admin::page_error(&self.admin),
            crate::pages::Page::Settings => crate::settings::page_error(&self.settings),
            // `FeedSnapshot.error`'s own doc comment already says "Page-level
            // error → `error-message`" — the field existed and
            // `inject_error_for_test` could set it, but no page arm ever read
            // it, so an injected (or, in production, a real background-fetch)
            // feed error painted nowhere.
            crate::pages::Page::Feed => self
                .feed
                .snapshot()
                .and_then(|s| s.error)
                .map(|e| e.resolve(fauna_i18n::strings::lookup)),
            _ => None,
        }
    }

    /// Text for the screen's `error-message` line as painted/registered: a
    /// test-agent injected error ([`Self::injected_error`], the cross-app
    /// `messages` state patch) wins while set, else the real
    /// [`Self::screen_error_text`]. One function so the paint, the registry,
    /// and the state protocol's `messages.error` cannot disagree — the
    /// state-vs-UI honesty contract.
    pub fn error_line_text(&self) -> Option<String> {
        self.injected_error
            .clone()
            .filter(|e| !e.is_empty())
            .or_else(|| self.screen_error_text())
    }

    /// Surface a **refused** test-agent command on the app's own `error-message`
    /// — `docs/goal/architecture/testing.md` convention 11: *"honour it or fail
    /// loudly on the app's own `error-message`"*.
    ///
    /// The agent acks every command (the driver's poll must terminate), so a
    /// `tracing::warn!` alone is **silent from the test's point of view**: the
    /// driver reads a green ack and the missing effect surfaces three steps later
    /// as a wrong-looking *product* assertion. That is exactly how tui's absent
    /// `conversations_seed_resolved_link_preview` handler presented — a card that
    /// "didn't paint" rather than a command that was never implemented. Writing
    /// the refusal here makes the *cause* the thing the test reads.
    ///
    /// Covers both refusal shapes `automation::apply_command` reports through
    /// `recognized: false` — an action no arm matches, and an arm that declined
    /// (bad payload, or a pre-auth manager that doesn't exist yet) — because to a
    /// driver they are the same failure: the command did not happen.
    ///
    /// Deliberately **not** localized: this text can only ever appear in a build
    /// carrying the automation surface (convention 15 compiles the agent out of
    /// release artifacts) and it is addressed to a test author, not a user.
    ///
    /// ⚠ **The slot matters more than the message, and two wrong slots were tried
    /// before this one** — record them, because each looked correct:
    ///
    /// 1. [`Self::injected_error`] — clears on **any nav patch** by design, and
    ///    `navigate_to` *is* a nav patch, so every action-layer helper wiped the
    ///    refusal microseconds after it was set. A mutation proved it: dropping the
    ///    `conversations_seed_resolved_link_preview` arm reddened its e2e at the
    ///    missing card with `error=''` — the surfacing *looked* built and reported
    ///    nothing.
    /// 2. The per-page [`Self::errors`] map — survives a *same-page* nav, but
    ///    `navigate_to("conversations")` after a refusal raised on the feed page
    ///    reads a different key and shows nothing. It only appeared to work because
    ///    the first case tested happened to re-navigate to the same page.
    ///
    /// So: a dedicated slot, page-independent (a refusal is not a property of a
    /// page) and nav-independent, cleared by [`Self::reset`] — which every `app`
    /// fixture calls before a test body, giving exactly one test of visibility.
    ///
    /// Known limitation, stated rather than hidden: while a launch/wizard surface
    /// owns the screen, [`Self::screen_error_text`] serves the onboarding machine's
    /// error instead, so a **pre-auth** refusal stays log-only. Writing over the
    /// wizard's own `error-message` would be worse than the gap — onboarding tests
    /// assert on that text — and the window is narrow, since every fixture logs in
    /// before driving commands. If a pre-auth refusal ever needs to be loud, that is
    /// a `launch-transient-error`-shaped addition, not a change here.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub fn report_refused_agent_command(&mut self, action: &str) {
        self.refused_agent_command = Some(format!(
            "test agent refused command {action:?}: not implemented on tui, \
             or its payload/preconditions were rejected"
        ));
    }

    /// Surface a command that was **honoured and then failed** — same slot, same
    /// reasoning, one distinction: a refusal means the app never tried, this
    /// means it tried and the op did not happen. Both are "the state the test
    /// asked for does not exist", which is why they share the slot and its
    /// precedence.
    ///
    /// The `reason` is the app's own error text (a backend refusal, a parse
    /// failure, a page error a UI gesture stamped instead of returning). Carrying
    /// it is the whole point: `tracing::error!` left the driver reading a green
    /// ack, so a nest `forbidden` surfaced two assertions later as a missing
    /// effect and was triaged as a half-completed MLS bootstrap for a full pass
    /// (`e2e-conventions.md` § convention 11).
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub fn report_failed_agent_command(&mut self, action: &str, reason: &str) {
        self.refused_agent_command =
            Some(format!("test agent command {action:?} failed: {reason}"));
    }

    /// The screen's pane title (chrome, not an automatable element).
    pub fn screen_title(&self) -> String {
        match self.launch {
            crate::launch::LaunchSurface::Wizard => self.wizard.title(),
            crate::launch::LaunchSurface::Unlock => {
                crate::unlock::title(crate::unlock::Mode::Unlock)
            }
            crate::launch::LaunchSurface::CreatePassphrase => {
                crate::unlock::title(crate::unlock::Mode::Create)
            }
            _ => self.launch.title(),
        }
    }

    /// Help text painted above the screen's elements (chrome).
    pub fn screen_description(&self) -> Vec<String> {
        match self.launch {
            crate::launch::LaunchSurface::Wizard => self.wizard.description(),
            crate::launch::LaunchSurface::Unlock => {
                crate::unlock::description(crate::unlock::Mode::Unlock)
            }
            crate::launch::LaunchSurface::CreatePassphrase => {
                crate::unlock::description(crate::unlock::Mode::Create)
            }
            _ => self.launch.description(),
        }
    }

    /// Route the app onto the unlock/create surface when the credential store
    /// resolved to the sealed headless backend and is still locked; otherwise
    /// run launch routing now. Called at boot (`main`) and after a factory
    /// reset re-locks the store ([`Self::reset`]) — `launch::start` reads the
    /// long-term store, so it must not run until the store can serve it.
    pub fn route_locked_store(&mut self, tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>) {
        match self.credentials.sealed_backend() {
            Some(sealed) if sealed.is_locked() => {
                self.launch = if sealed.file_exists() {
                    crate::launch::LaunchSurface::Unlock
                } else {
                    crate::launch::LaunchSurface::CreatePassphrase
                };
                self.focus = 0;
            }
            _ => crate::launch::start_or_offer_chooser(self, tx),
        }
    }

    /// Whether the focus ring is in the sidebar (authenticated only — the
    /// unauthenticated screen paints no sidebar, so it is always page-zoned).
    fn in_sidebar(&self) -> bool {
        self.authenticated() && self.zone == Zone::Sidebar
    }

    /// Clamp the focus ring onto a focusable element of the current page.
    ///
    /// One background custody-ceremony drive pass (T16 — the record-then-act
    /// "act" half), assembled from whatever session pieces exist right now.
    /// Missing pieces degrade, never block: no conversations session skips
    /// (posts wait owed in the account plane), no account store rides the
    /// `NoStoreWriter` (row writes stay owed for the `AccountStoreReady`
    /// edge's re-drive).
    fn spawn_custody_drive(&self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some(secret) = self.settings.identity_secret() else {
            return;
        };
        crate::custody_glue::spawn_drive(
            Arc::clone(&session.client),
            secret,
            self.conversations.real_session.clone(),
            self.settings.account_store.clone(),
        );
    }

    /// Start the share plane's glue task (`share_glue::run`) at the
    /// `AccountStoreReady` edge. Every absent piece is a legitimate degraded
    /// session state (no agent surface, no conversations engine, no config
    /// store, no app dir), and each means the plane honestly cannot run —
    /// skipped with a debug line, never a broken half-start.
    #[cfg(feature = "p2p-share")]
    fn spawn_share_glue(
        &mut self,
        account: fauna_sync_engine::account_runtime::AccountStoreHandle,
    ) {
        let (Some(session), Some(secret_hex), Some(conversations), Some(agent)) = (
            self.session.as_ref(),
            self.settings.offline_share.secret_hex.clone(),
            self.conversations.real_session.clone(),
            self.sync_agent.share_access(),
        ) else {
            tracing::debug!("share glue: a session seam is absent; the plane stays down");
            return;
        };
        let Some(app_dir) = crate::session::config_dir() else {
            tracing::debug!("share glue: no app dir for the spool; the plane stays down");
            return;
        };
        let cell = crate::share_glue::SharePlaneCell::default();
        self.settings.share_plane = Some(std::sync::Arc::clone(&cell));
        tokio::spawn(crate::share_glue::run(crate::share_glue::ShareGlue {
            nest: Arc::clone(&session.client),
            secret_hex,
            account,
            agent,
            conversations,
            tx: self.tx.clone(),
            session_seat: self.settings.offline_share.session_seat.clone(),
            cell,
            spool_root: app_dir.join("share-spool"),
        }));
    }

    /// Only the page zone needs clamping: the sidebar's ring position *is*
    /// [`Self::page`], which is a `Page` and therefore always in range.
    pub fn clamp_focus(&mut self) {
        let n = self.page_focusable_count();
        if n == 0 {
            self.focus = 0;
        } else if self.focus >= n {
            self.focus = n - 1;
        }
    }

    fn page_focusable_count(&self) -> usize {
        self.page_elements()
            .iter()
            .filter(|e| e.focusable())
            .count()
    }

    /// Move the ring forward within the active zone. In the sidebar that means
    /// the next page (selection *is* navigation, clamped at the ends, like the
    /// linux `gtk::ListBox`); in the page pane, the next focusable element
    /// (wrapping, like every wizard page).
    pub fn focus_next(&mut self) {
        if self.in_sidebar() {
            let rows = self.sidebar_pages();
            // Clamp at the ends (like the linux `gtk::ListBox`): the next visible
            // row, or stay put when already on the last one.
            if let Some(next) = rows
                .iter()
                .position(|p| *p == self.page)
                .filter(|&i| i + 1 < rows.len())
                .map(|i| rows[i + 1])
            {
                self.set_page(next);
                self.clamp_focus();
            }
            return;
        }
        let n = self.page_focusable_count();
        if n > 0 {
            self.focus = (self.focus + 1) % n;
        }
    }

    pub fn focus_prev(&mut self) {
        if self.in_sidebar() {
            let rows = self.sidebar_pages();
            if let Some(prev) = rows
                .iter()
                .position(|p| *p == self.page)
                .filter(|&i| i > 0)
                .map(|i| rows[i - 1])
            {
                self.set_page(prev);
                self.clamp_focus();
            }
            return;
        }
        let n = self.page_focusable_count();
        if n > 0 {
            self.focus = (self.focus + n - 1) % n;
        }
    }

    /// The element the focus ring currently sits on.
    ///
    /// In the sidebar the ring has no index of its own — it *is* the selected
    /// page, so there is no second piece of state that could drift from the
    /// painted highlight.
    pub fn focused(&self) -> Option<crate::element::Element> {
        if self.in_sidebar() {
            return Some(crate::element::Element::tab(self.page));
        }
        self.page_elements()
            .into_iter()
            .filter(|e| e.focusable())
            .nth(self.focus)
    }

    /// The ABSOLUTE position of the focused element within [`Self::page_elements`]
    /// — unlike [`Self::focused`]'s returned `Element`, this survives even when
    /// several elements share one id (or no id at all).
    ///
    /// Paint must key focus off this, not off `id`: `settings/root.rs`'s rail
    /// deliberately gives most rows a blank id (no ui.yaml rail-row id is scoped
    /// for them), so comparing by id marked EVERY blank-id row focused the
    /// moment focus landed on any one of them — a live user's "all menu choices
    /// except the top 2-3 are highlighted together." `None` in the sidebar zone:
    /// the sidebar's own ring paints through [`Self::sidebar_index`], not this.
    pub fn focused_index(&self) -> Option<usize> {
        if self.in_sidebar() {
            return None;
        }
        self.page_elements()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.focusable())
            .nth(self.focus)
            .map(|(i, _)| i)
    }

    /// Change [`Self::page`], applying the shell-state reset (Settings → rail
    /// Root, Admin → Dashboard, Events → List/today) exactly once per genuine
    /// edge — the ONE door every `self.page` mutation must go through.
    ///
    /// Why a dedicated door: in the sidebar "selection *is* navigation" — moving
    /// the ring (`focus_next`/`focus_prev`) and a mouse click
    /// ([`Self::click_sidebar`]) both mutate `self.page` directly, live-previewing
    /// the destination page before any gesture fires. [`Self::apply`] used to
    /// compute its OWN `edge = self.page != page` — but by the time a sidebar
    /// actuation (Enter or click) reaches `apply`, `self.page` already equals the
    /// target (the ring/click set it first), so that edge check always read
    /// `false` and the resets never fired via the sidebar. A user leaving a
    /// settings sub-page (e.g. Folders) and returning to Settings through the
    /// sidebar landed right back on the sub-page instead of the rail Root —
    /// "stuck there forever" from a live user's report. Routing every
    /// `self.page` write through this one function, called BEFORE the mutation
    /// happens, makes the edge check see the real prior page every time,
    /// regardless of which path (ring preview, click, or a committed `apply`)
    /// triggers it.
    fn set_page(&mut self, page: Page) {
        let edge = self.page != page;
        // The report sheet and its acknowledgement belong to the page they
        // opened over; leaving it closes the one and retires the other.
        crate::report::on_nav(&mut self.report, page);
        // The settings shell lands on its rail Root on the nav edge whether or
        // not there is a session — the sub-page is shell state, not session
        // state. Reset only on the *edge*, so re-selecting a sub-page on the
        // same visit isn't clobbered.
        if page == Page::Settings && edge {
            self.settings.sub = crate::settings::SubPage::Root;
        }
        // The admin shell lands on its Dashboard on the nav edge, the same
        // shell-state (not session-state) reset the Settings rail does above — so
        // entering via the `admin-tab` sidebar (this path) always lands on the
        // Dashboard, and the two-element nav patch then routes to a sub-page.
        if page == Page::Admin && edge {
            self.admin.sub = crate::admin::AdminPage::Dashboard;
        }
        // The events shell lands back on List/today on the nav edge — the same
        // shell-state (not session-state) reset the two above do, and moved here
        // from `on_nav_enter` for the same reason: a same-page re-enter is a
        // RELOAD, and must not throw its caller out of the view they are in.
        if page == Page::Events && edge {
            crate::events::on_nav_enter(&mut self.events);
        }
        // The conversations tab lands on the thread list — on the edge and on a
        // re-select alike, unlike the three above. The cross-app contract of
        // "navigate to conversations" is that the list pane is showing (every
        // app's e2e waits on `new-conversation-button` after it), which the
        // two-pane apps satisfy for free and a list-OR-thread shell has to
        // arrange; and ui.yaml gives `conversation_detail` no dismiss element,
        // so without this the tab is no way back at all and only Esc is.
        // Search and profile open a thread by writing `self.page` themselves,
        // never through here, so their deep links are not undone.
        if page == Page::Conversations {
            crate::conversations::show_list(&mut self.conversations);
        }
        self.page = page;
    }

    /// **The nav-edge hook for a pending stolen-identity persist-failure
    /// message.** Discharges it (`PageErrors::acknowledge_stolen_failed_message`)
    /// the instant `(self.page, self.settings.sub)` is no longer
    /// `(Settings, Account)` — the acknowledgment gesture `settings.md` §
    /// Recovery kit → *The persist-failure message survives the page* defines
    /// (no new `ui.yaml` element: leaving the Account sub-page IS the
    /// acknowledgment).
    ///
    /// Edge-triggered on the (page, sub) PAIR rather than wired into any one
    /// nav call site, because there isn't one: `Action::NavBack`, Esc, the
    /// rail `Open*` handlers, `route_subpage`, `set_page`, and a few direct
    /// field writes elsewhere (an account switch, `clear_session`) all change
    /// `self.page`/`self.settings.sub` with no single shared door between
    /// them. Instead this
    /// is called from the two places that between them cover every one of
    /// those in the running app — the end of [`Self::handle_key`] (keyboard,
    /// including the direct Esc arm that bypasses `gesture_work` entirely)
    /// and the end of [`gesture_work`] (mouse clicks via `actuate_focused`
    /// and the e2e agent both actuate through it, "the one gesture door") —
    /// plus once per frame in the render loop (`main.rs`, right before
    /// `terminal.draw`) as the backstop for any remaining programmatic
    /// transition neither of those two catches. Calling it more than once
    /// for the same (page, sub) is a harmless no-op.
    pub(crate) fn sync_recovery_message_nav_edge(&mut self) {
        let now_on_account =
            self.page == Page::Settings && self.settings.sub == crate::settings::SubPage::Account;
        if self.was_on_settings_account && !now_on_account {
            self.errors.acknowledge_stolen_failed_message();
            self.stolen_outcome_on_screen = false;
            // The user has had the whole visit to copy the key; the dead
            // session it was parked over may now go the ordinary way.
            self.escalate_deferred_supersession();
        }
        self.was_on_settings_account = now_on_account;
    }

    /// Read-only half of [`Self::defer_own_supersession`]: would a supersession
    /// arriving now be this device's own ceremony's? For the callers that must
    /// ask before they may borrow `self` mutably — `launch::route`'s match guard,
    /// the second channel the refusal arrives by.
    pub(crate) fn owns_its_supersession(&self) -> bool {
        self.stolen_ceremony_in_flight
            || self.stolen_outcome_on_screen
            || self.errors.stolen_failed_pending()
    }

    /// Whether a mid-session supersession belongs to this device's OWN
    /// stolen-identity ceremony and must wait — recording it if so.
    ///
    /// The ceremony is what supersedes the identity, so its session's next auth
    /// refresh is refused the instant the nest commits, typically before the
    /// ceremony's own result is folded. Escalating then bumps the session
    /// generation, the fold is dropped as stale, and — when the successor's
    /// seed could not be stored — the only copy of the new key is never shown
    /// (`settings.md` § Recovery kit → *The persist-failure message survives
    /// the page*: it wins over every other writer until the user leaves
    /// Account, and an escalation to the launch surface is the widest writer
    /// there is). So while the ceremony runs, or its persist-failure message is
    /// still parked, the escalation is owed rather than performed.
    pub(crate) fn defer_own_supersession(&mut self) -> bool {
        let owned = self.owns_its_supersession();
        if owned {
            self.superseded_deferred = true;
        }
        owned
    }

    /// Perform a supersession escalation [`Self::defer_own_supersession`] held
    /// back, once the flow that owned the screen is done. A no-op when none is
    /// owed.
    pub(crate) fn escalate_deferred_supersession(&mut self) {
        if std::mem::take(&mut self.superseded_deferred) {
            self.escalate_to_launch_surface();
        }
    }

    /// The ceremony adopted its successor and is switching to it: the switch is
    /// itself the full relaunch, so an owed escalation is spent, not performed.
    pub(crate) fn forget_deferred_supersession(&mut self) {
        self.superseded_deferred = false;
    }

    /// Apply a navigation actuation (a sidebar click, or the `nav` state patch).
    /// The new page has a different element count, so the ring must re-seat.
    ///
    /// Hands back the nav-edge refresh the destination implies ([`on_nav_enter`])
    /// rather than firing it — same duality as [`gesture_work`], same reason: the
    /// agent's nav must **await** it (so the driver's ready-ack contract
    /// guarantees the fetch landed before the next single-shot read), while the
    /// keyboard's spawns it.
    #[must_use = "the nav-edge refresh must be awaited (agent) or spawned (keyboard), or the page shows stale data"]
    pub fn apply(&mut self, page: Page) -> Option<PageOp> {
        // The `profile-tab` (and any `nav` patch without an actor_id) always opens
        // the viewer's OWN profile, resetting any prior contact-row tap-through —
        // the one nav that reaches `apply(Profile)` (an OTHER profile is opened
        // through `open_profile(Some(hex))`, which never routes here). Route it
        // through the same open door so the SELF reset + refetch always fire, even
        // when already on an OTHER profile (`self.page` is already `Profile`).
        if page == Page::Profile && self.authenticated() {
            self.open_profile(None);
            return None;
        }
        self.set_page(page);
        self.clamp_focus();
        // **A same-page nav re-reads the page.** Only the shell-state resets
        // above are edge-scoped; the data refetch fires either way, because
        // "navigate to the tab I am already on" is how every app's e2e spells
        // *reload* (`actions/*.py`: `reload = navigate`, documented as "a fresh
        // Page_Loaded -> LoadAsync refetch"), and a real user re-selecting their
        // current tab means the same thing.
        //
        // tui used to gate the whole hook on `edge`, which made every `reload()`
        // a silent no-op here while the other apps refetched — the exact shape
        // convention 11 forbids, since the driver reads a green ack as "the app
        // did what I asked". It cost this slice a full e2e cycle: a ward's
        // usage readout sat at 0 after a heartbeat that had demonstrably landed,
        // because the page never asked the nest again.
        self.authenticated()
            .then(|| on_nav_enter(self, page))
            .flatten()
    }

    /// Open a profile **detail** view for `target` — `None` = the viewer's own
    /// (the `profile-tab`), `Some(hex)` = another actor (a contact-row tap or a
    /// `nav` patch carrying an `actor_id`). The linux `open_profile(Option<hex>)`
    /// twin: a fresh target resets the per-view state and re-fetches (no
    /// client-side cache — `ui/profile.md` § Persistence). The open-time reads
    /// ride the channel; the driver's reads after a nav are all polling.
    pub fn open_profile(&mut self, target: Option<String>) {
        self.profile.viewing = target;
        self.profile.reset_for_open();
        self.profile.overlays = crate::contacts::overlay_projection(self);
        if self.authenticated() {
            crate::profile::spawn_open_refresh(
                &self.profile,
                &self.tx,
                // For the page-path peer-anchor harvest's re-drive: a fresh
                // seed must announce to the live witness, or the once-per-
                // seed-generation anchor read stays stale for the session.
                self.conversations
                    .real_session
                    .as_ref()
                    .map(std::sync::Arc::downgrade),
                self.session_generation,
            );
        }
        self.page = Page::Profile;
        self.clamp_focus();
    }

    /// Read an editable field — the one door paint, `/element/type` and the
    /// keyboard all read through, so they can never disagree about a value.
    ///
    /// One exhaustive match, no fallback arm: field ownership is carried on
    /// the [`crate::element::Field`] type itself, so a new variant that names
    /// no owning page fails to compile here rather than silently falling
    /// through to the wizard (`apps/tui.md` § Target state).
    pub fn field(&self, field: crate::element::Field) -> String {
        use crate::element::Field;
        match field {
            Field::Wizard(f) => self.wizard.field(f),
            Field::Feed(f) => crate::feed::field(&self.feed, &f),
            Field::Conversations(f) => crate::conversations::field(&self.conversations, &f),
            Field::Contacts(f) => crate::contacts::field(&self.contacts, &f),
            // The RAW buffer — the keyboard and the agent's `type` append to
            // it. Only the painted/registered element text is masked
            // (`crate::unlock::elements`).
            Field::Unlock(f) => crate::unlock::field(&self.unlock, &f),
            Field::Locked(f) => crate::locked::field(&self.locked, &f),
            Field::Profile(f) => crate::profile::field(&self.profile, &f),
            Field::Events(f) => crate::events::field(&self.events, &f),
            Field::Settings(f) => crate::settings::field(&self.settings, &f),
            Field::Search(f) => crate::search::field(&self.search, &f),
            Field::Media(f) => crate::media::field(&self.media, &f),
            Field::Nostr(f) => crate::nostr::field(&self.nostr, &f),
            Field::Admin(f) => crate::admin::field(&self.admin, &f),
            Field::Moderation(f) => crate::moderation::field(&self.moderation, &f),
            Field::Report(f) => crate::report::field(&self.report, &f),
            Field::Bridges(f) => crate::bridges::field(&self.bridges, &f),
            Field::Family(f) => crate::family::field(&self.family, &f),
            Field::Backups(f) => crate::backups::field(&self.backups, &f),
        }
    }

    /// Write an editable field (see [`Self::field`]).
    ///
    /// Returns the network work the write implies, if any — a feed search
    /// re-query or a conversations recipient-resolve, unified as a
    /// [`PendingWrite`]. The caller decides how to run it: the agent's path
    /// awaits it (element reads are single-shot, so the effect must have landed
    /// before the reply), the keyboard's spawns it (a slow nest must never
    /// freeze the render loop).
    ///
    /// One exhaustive match, no fallback arm — see [`Self::field`].
    #[must_use = "a pending write (re-query / resolve) must be awaited or spawned, or it never runs"]
    pub fn set_field(
        &mut self,
        field: crate::element::Field,
        value: String,
    ) -> Option<PendingWrite> {
        use crate::element::Field;
        match field {
            Field::Wizard(f) => {
                self.wizard.set_field(f, value);
                None
            }
            Field::Feed(f) => {
                crate::feed::set_field(&mut self.feed, f, value).map(PendingWrite::FeedSearch)
            }
            Field::Conversations(f) => {
                crate::conversations::set_field(&mut self.conversations, f, value)
                    .map(PendingWrite::ConvResolve)
            }
            Field::Contacts(f) => {
                crate::contacts::set_field(&mut self.contacts, f, value);
                None
            }
            Field::Unlock(f) => {
                crate::unlock::set_field(&mut self.unlock, f, value);
                None
            }
            Field::Locked(f) => {
                crate::locked::set_field(&mut self.locked, f, value);
                None
            }
            Field::Profile(f) => {
                crate::profile::set_field(&mut self.profile, f, value);
                None
            }
            Field::Events(f) => {
                crate::events::set_field(&mut self.events, f, value);
                None
            }
            Field::Settings(f) => {
                crate::settings::set_field(&mut self.settings, f, value);
                None
            }
            Field::Search(f) => {
                crate::search::set_field(&mut self.search, f, value);
                None
            }
            Field::Media(f) => {
                crate::media::set_field(&mut self.media, f, value);
                None
            }
            Field::Admin(f) => {
                crate::admin::set_field(&mut self.admin, f, value);
                None
            }
            Field::Moderation(f) => {
                crate::moderation::set_field(&mut self.moderation, f, value);
                None
            }
            Field::Report(f) => {
                crate::report::set_field(&mut self.report, f, value);
                None
            }
            Field::Nostr(f) => {
                crate::nostr::set_field(&mut self.nostr, f, value);
                None
            }
            Field::Bridges(f) => {
                crate::bridges::set_field(&mut self.bridges, f, value);
                None
            }
            Field::Family(f) => {
                crate::family::set_field(&mut self.family, f, value);
                None
            }
            Field::Backups(f) => {
                crate::backups::set_field(&mut self.backups, f, value);
                None
            }
        }
    }

    /// Dispatch a wizard gesture: validate client-side, then run it.
    ///
    /// `run` decides whether the machine call is awaited (the agent's click
    /// path, so the driver's next read sees the new step) or spawned (the
    /// keyboard path, so a slow probe never freezes the render loop). Both go
    /// through this one function — the "activate invokes the same method"
    /// convention.
    pub fn dispatch_wizard(
        &mut self,
        action: crate::wizard::Action,
        run: impl FnOnce(
            std::sync::Arc<fauna_onboarding_machine::OnboardingMachine>,
            crate::wizard::Action,
            String,
        ),
    ) {
        match self.wizard.prepare(&action) {
            Ok(payload) => {
                self.wizard.error = None;
                run(std::sync::Arc::clone(&self.wizard.machine), action, payload);
            }
            Err(message) => self.wizard.error = Some(message),
        }
    }

    /// The e2e `reset` command: a factory reset. Sign out, clear the credential
    /// namespace, drop the wizard's typed state, and return to the fresh launch
    /// state. The connection watch pump ends with the dropped client; the status
    /// line goes back to the honest signed-out `Disconnected`.
    ///
    /// The launch surface returns to `Wizard` (a reset app has no saved account
    /// to silently challenge) and the machine is dropped with it — a stale one
    /// would still answer `Retry` against the pre-reset nest.
    ///
    /// Clearing the store is what makes the *next launch* a fresh install, and
    /// it must be wholesale: see `fauna_credential_store::cred_file_remove` for
    /// why leaving `fauna/index` behind 403-hangs the next sign-in. windows
    /// (`store.DeleteAll()`), android (`storage.clear()`) and linux
    /// (`delete_credentials`) all wipe the namespace here too.
    ///
    /// The wipe is synchronous on purpose. The keyring arm hops to its own OS
    /// thread and joins, so it is legal from this tokio worker, and blocking
    /// until it lands is exactly what the single-shot element-read contract
    /// wants: `/app/commands` must not reply before the reset has settled.
    /// It runs as a continuation of the account runtime's stop
    /// ([`Self::after_stops`]), so this returns before it when a stop is in
    /// flight — which is why the loop holds a command's ack until
    /// [`Self::teardown_pending`] clears (`main.rs`).
    ///
    /// Order matters: every writer that could touch the store is dropped
    /// *before* the wipe, never after. A namespace delete is not atomic against
    /// a concurrent write (`keyring_delete_namespace`), so a machine still
    /// persisting a silent-challenge result would land its slots behind the
    /// sweep and resurrect the identity the reset just destroyed.
    pub fn reset(&mut self) {
        // Tear down every per-identity writer + manager first (this drops the WS
        // supervisor and the launch machine — the only things that persist to the
        // credential store), THEN wipe the namespace: a namespace delete is not
        // atomic against a concurrent write, so a machine still persisting a
        // silent-challenge result must be gone before the sweep or it would
        // resurrect the identity the reset just destroyed.
        self.drop_authenticated_state(fauna_client_account_runtime::StopReason::SignOut);
        // A refused-command banner is scoped to ONE test, and `reset()` is what the
        // `app` fixture calls before each test body — so this is the clear point
        // that stops a refusal leaking into the next test of a reused app process
        // (`Self::report_refused_agent_command`).
        self.refused_agent_command = None;
        // Same one-test lifetime, same clear point (`Self::barrier_probe`).
        self.barrier_probe = None;
        self.barrier_ack_probe = None;
        // `session_generation` is deliberately NOT cleared here, unlike the two
        // probe slots above. A reset IS a teardown — it just counted itself via
        // `drop_authenticated_state` — so zeroing it would erase the very event
        // the counter reports. Tests read a DELTA across their own gesture, so
        // an accumulating value costs them nothing, and "starts at 0 for a fresh
        // process, only ever increases" stays literally true on all three apps
        // (`fauna_e2e_agent::SESSION_GENERATION_KEY`).
        // A reset destroys every identity on the box, so the ceremony result
        // that `drop_authenticated_state` deliberately preserved has nothing
        // left to describe — this is its clear point (and the one the `app`
        // fixture calls, so a sweep cannot leak into the next test).
        self.succession_sweep = None;
        // The defer is a decision about that sweep, so it clears with it —
        // otherwise a fresh identity on a reset box would start with the review
        // pass already dismissed by someone who no longer exists.
        self.succession_review_deferred = false;
        // Same reasoning one line up: a reset destroys every identity on the
        // box, so there is no successor left to owe a kit to — and no
        // predecessor seed left in the registry for that kit to have sealed.
        self.succession_kit_owed = false;
        self.succession_predecessor = None;
        // Everything below waits for the account runtime's stop: the erase
        // must never meet a store still open, and the wizard must not come up
        // (and persist a new identity) before the sweep below has passed. The
        // UI loop keeps running meanwhile, refusing input (`teardown_pending`).
        self.after_stops(Self::erase_after_sign_out);
    }

    /// The half of [`Self::reset`] that must follow the account runtime's stop
    /// — every line of it: the erase, the residue report, the wizard.
    fn erase_after_sign_out(&mut self) {
        // Erase every known account's scoped local state (account-scoping.md
        // § Serialized switching, "Erasure follows scope") BEFORE the
        // credential namespace is wiped below — this reads the registry to
        // know which actors existed; reading it after would find an
        // already-empty registry and erase nothing.
        let survivors = crate::account_scope::erase_all_known_accounts(self);
        // …and the credential half, for the same reason one namespace lower.
        // The namespace wipe erases `fauna-tui` and CANNOT reach the shared
        // `fauna-account-store` namespace where this machine's writer key and
        // each account's principal bundle live — a credential key is a
        // (namespace, key) pair and the backend binds the namespace into the
        // physical row — so the registry's per-actor sweep runs first, then the
        // wipe, then a read-back of what both left: one shared sequence
        // (`fauna_credential_store::erase_all_credentials`,
        // `long-term-store.md` § Cleanup contract). After
        // `erase_all_known_accounts`, which enumerates through the same registry
        // index the wipe destroys.
        let credentials = fauna_credential_store::erase_all_credentials(
            &crate::session::registry(self),
            &self.credentials,
        );
        // The erase is best-effort BY DESIGN — a sign-out completes even when a
        // scope or a credential will not go (`account-scoping.md` § Erasure
        // follows scope), and that must not be reversed. What it must not do is
        // look identical to a sign-out that removed everything, so the residue
        // is reported both ways: paths and key names to the log, the fact to
        // the user.
        //
        // Built HERE, after BOTH halves, and nowhere earlier. This line used to
        // be written straight after the filesystem erase, so it answered for
        // half a sign-out: a keyring that refused the wipe was a `warn!` below
        // it, painted over by a clean "Signed out" (2026-09-13). And it must
        // follow `drop_authenticated_state` above, which is what clears the three
        // injected lines, so nothing earlier in the teardown can wipe it. It is
        // RECORDED (install-scoped, so it outlives this process) and painted as
        // `identity_choice`'s `sign-out-residue` view — the surface this reset
        // lands on (`account_scope::record_residue`).
        self.sign_out_residue = crate::account_scope::record_residue(&survivors, &credentials);
        self.launch = crate::launch::LaunchSurface::Wizard;
        // On the sealed headless arm the wipe above also RELOCKED the store
        // (the file is gone; the next onboarding re-chooses its passphrase),
        // so the create surface must come back before the wizard can persist
        // anything. The keyring/file arms fall straight through to the wizard
        // — `route_locked_store` only reroutes a locked sealed backend, and a
        // fresh-reset launch has nothing for `launch::start` to challenge, so
        // this deliberately does NOT re-run launch routing on those arms.
        self.unlock = crate::unlock::UnlockState::default();
        if self.credentials.needs_unlock() {
            self.launch = crate::launch::LaunchSurface::CreatePassphrase;
        }
    }

    /// The two signals **every** identity teardown owes, however shallow it is —
    /// the counter convention 14's negative asserts read, and the identity-scoped
    /// alert registry (`critical-alerts.md` § Mechanism → *Lifetime*).
    ///
    /// ⚠ **This exists because "the teardown function clears the alerts" is not
    /// the same claim as "every teardown clears the alerts".** tui shipped the
    /// clear inside [`Self::drop_authenticated_state`] and was still wrong: the
    /// two mid-session escalation arms ([`Self::escalate_to_launch_surface`])
    /// tear the session down *without* that full teardown, so they skipped both
    /// signals — one leaked sweep loop per re-trust, still polling the public
    /// PLC directory for a departed identity, and a relaunch that never counted
    /// itself. A partial teardown is the shape a reviewer reading only
    /// `drop_authenticated_state` calls correct. So: **any arm that ends the
    /// authenticated session calls this, and only this is mandatory.**
    ///
    /// Both signals are emitted at *initiation*, before any state is dropped.
    /// For the counter that is what makes "did this gesture relaunch me?"
    /// answerable from the counter alone
    /// (`fauna_e2e_agent::SESSION_GENERATION_KEY`); for the registry it means the
    /// sweep loop is told to stop *before* the client it holds is dropped.
    ///
    /// The `clear_all` half is doing two jobs at once, and the second is easy to
    /// miss: alerts are keyed to the departing identity's DID
    /// (`atproto-custody:<did>`) and nothing re-checks a departed DID, so one
    /// left standing accuses an account the user no longer has, on every page,
    /// un-dismissably — **and** the call bumps `CriticalAlerts::teardown_epoch`,
    /// the only stop signal `critical_alerts::spawn_session_start_sweep`'s
    /// 6-hourly loop watches.
    fn begin_identity_teardown(&mut self) {
        self.session_generation = self.session_generation.saturating_add(1);
        self.alerts.clear_all();
        // Whatever ceremony was in flight belongs to the departing session:
        // its result is now stale and will be dropped, so nothing may go on
        // deferring a supersession on its behalf.
        self.stolen_ceremony_in_flight = false;
        self.stolen_outcome_on_screen = false;
    }

    /// Run `then` once every teardown stop in flight has finished — inline
    /// when none is, which is every teardown before an account runtime was
    /// ever assembled.
    ///
    /// `session::sign_out` drops the session synchronously but spawns the
    /// waits (the agent's un-provision, the account runtime's stop), so a
    /// caller hands this everything that must follow them: the erase, which
    /// must never meet a store still open (`account-scoping.md` § Erasure
    /// follows scope), and the next session's launch. A second teardown
    /// landing mid-stop queues behind the first, never alongside it
    /// (`fauna_client_account_runtime::StopQueue`).
    pub(crate) fn after_stops(&mut self, then: impl FnOnce(&mut App) + 'static) {
        self.stops.push(Box::new(then));
        self.run_ready_after_stops();
    }

    /// Run each continuation that is free to run. Popped one at a time, so a
    /// continuation that itself tears down (a switch's launch whose sign-in
    /// fails and escalates) holds back everything still queued behind it.
    fn run_ready_after_stops(&mut self) {
        while let Some(next) = self.stops.next_ready() {
            next(self);
        }
    }

    /// Whether a teardown is still under way — a stop in flight, or work
    /// queued behind one. For exactly this long the loop refuses input to the
    /// outgoing session: keys, clicks and e2e requests alike (`main.rs`),
    /// since a gesture now would act on an identity being torn down, or —
    /// a quit — leave a sign-out that never erased.
    pub fn teardown_pending(&self) -> bool {
        self.stops.is_busy()
    }

    /// A mid-session verdict the session cannot survive: drop it and re-enter the
    /// real launch flow, **keeping the credentials**. Shared by
    /// [`DataMessage::NestIdentityChanged`] and [`DataMessage::IdentitySuperseded`]
    /// — see those arms for why each escalates rather than toasting.
    ///
    /// ⚠ **Deliberately NOT [`Self::drop_authenticated_state`], and that is
    /// measured, not assumed:** routing these arms through it reds
    /// `test_nest_identity_pin_post_auth.py::test_post_auth_valid_refresh_does_not_escalate`,
    /// because that teardown ends in `settings.clear_session()`, which zeroes the
    /// identity secret the launch-flow re-entry still needs (re-trust, and the
    /// successor/re-import reasoning, both read it). What these arms owe is the
    /// identity-scoped clear plus the count — [`Self::begin_identity_teardown`] —
    /// never the per-page teardown.
    ///
    /// Re-entering the real launch flow (rather than synthesizing a surface in
    /// place) is what keeps the surface's affordances live: the button on it
    /// drives the machine that produced the verdict, so a synthesized twin would
    /// render identically with a button that silently does nothing.
    /// A refused identity whose **verified** successor this device already
    /// holds the key to: adopt it and sign in, rather than asking the user to
    /// import a key the device has. Returns `true` when the switch was started.
    ///
    /// The state it resolves is the one a lost succession reply leaves behind
    /// (`identity-succession.md` § Implementation status today, *a lost submit
    /// reply no longer destroys the account*): the ceremony persists the minted
    /// successor before anything can fail, so when its outcome could not be
    /// confirmed the successor's seed sits in the account store with the old
    /// identity still active. The undecidable arm's message promises that
    /// *reopening the app signs in as it* — and a relaunch lands here, refused
    /// as superseded, with the chain naming exactly that successor.
    ///
    /// Safe to do without asking because both halves are proofs, not claims:
    /// `successor` is what the registration chain authorizes
    /// (`launch::verify_superseded_successor`, never the nest's say-so), and
    /// the key is one this device minted and kept. It is the import the user
    /// would perform by hand, minus a secret they were never shown. Whether to
    /// adopt — and the succession link it records, which the corpus re-seal
    /// needs at every later sign-in — is the shared
    /// `AccountRegistry::adopt_held_successor`, the one decision every app's
    /// launch route calls.
    ///
    /// What stays here is the obligation the ceremony never reached: the
    /// successor is owed its recovery kit — the old kit retired with the old
    /// identity, so without it the account would stay kitless. The group sweep
    /// is NOT re-run: it needs the old identity's engine, which a refused
    /// launch never opens.
    fn adopt_held_successor(&mut self, predecessor: &str, successor: &str) -> bool {
        if !crate::session::registry(self).adopt_held_successor(predecessor, successor) {
            return false;
        }
        self.succession_predecessor = Some(predecessor.to_string());
        self.succession_kit_owed = true;
        match self.switch_account(successor, false) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("[launch] adopting the held successor {successor} failed: {e}");
                false
            }
        }
    }

    fn escalate_to_launch_surface(&mut self) {
        self.begin_identity_teardown();
        crate::session::sign_out(
            self,
            fauna_client_account_runtime::StopReason::AccountSwitch,
        );
        self.launch_machine = None;
        // The re-entry signs in again, so it waits for the stop: one account's
        // runtime at a time.
        self.after_stops(|app| {
            let tx = app.tx.clone();
            crate::launch::start(app, &tx);
            app.focus = 0;
        });
    }

    /// Drop every piece of authenticated, per-identity state — the teardown
    /// shared by a full [`Self::reset`] (which then wipes the credential
    /// namespace) and an account switch ([`Self::switch_account`], which keeps
    /// it). Store-writers first (`sign_out` drops the WS supervisor;
    /// `launch_machine = None` drops the machine), then every per-actor manager
    /// reset to its signed-out default so a stale one can't keep answering
    /// snapshots for the identity that just went away. Deliberately does NOT
    /// touch the credential namespace, the launch surface, or the unlock state —
    /// each caller owns those.
    ///
    /// `pub(crate)` so a page module can pin its OWN half of this teardown from
    /// its own tests — the invariant that matters is per-page (a page holding
    /// key material must not survive the identity that minted it), so the
    /// assertion belongs beside that page, not in one omnibus test here that
    /// the next page's author would have to remember to extend.
    pub(crate) fn drop_authenticated_state(
        &mut self,
        reason: fauna_client_account_runtime::StopReason,
    ) {
        // Count the teardown and drop the alert registry BEFORE doing any of it
        // — see `begin_identity_teardown` for why both are emitted at
        // initiation, and why they live there rather than inline here (the two
        // escalation arms tear down without reaching this function at all).
        self.begin_identity_teardown();
        crate::session::sign_out(self, reason);
        self.launch_machine = None;
        self.page = Page::ALL[0];
        self.zone = Zone::Sidebar;
        self.feed = crate::feed::FeedState::default();
        self.conversations = crate::conversations::ConversationsState::default();
        // Also drops the per-actor owner `BackupKey` the Media page derived — it
        // must never outlive the identity that went away.
        self.media = crate::media::MediaState::default();
        self.notifications = crate::notifications::NotificationsState::default();
        // Drops the queue AND the client Arc — the queue is the caller's OWN
        // flagged content, so none of it may survive the identity going away.
        self.moderation = crate::moderation::ModerationState::default();
        self.report = crate::report::ReportState::default();
        self.profile = crate::profile::ProfileState::default();
        self.search = crate::search::SearchState::default();
        // Drops the bridge status AND the client Arc — an unlinked-vs-linked
        // state from the previous identity must never paint for the next one.
        self.nostr = crate::nostr::NostrState::default();
        // Same for the unified Bridges page: the filtered rows + follows + the
        // client Arc must not outlive the identity that fetched them.
        self.bridges = crate::bridges::BridgesState::default();
        // Same for Backups — and here the drop is load-bearing beyond staleness:
        // the state holds the owner's identity seed (the `NestBackupKey` and the
        // account-plane seal both derive from it), which must never outlive the
        // identity that went away.
        self.backups = crate::backups::BackupsState::default();
        // Drop the admin gate + shell state: the *next* login re-runs the gate
        // check fail-closed, so a stale `true` must never linger to flash the
        // `admin-tab` row for a non-admin (a switch to a non-admin account).
        self.am_i_admin = false;
        self.admin = crate::admin::AdminState::default();
        // Same fail-closed drop for the family gate + page: the next login
        // re-runs `fauna.family.status`, so a stale `true` must never linger to
        // flash the `family-tab` row (or the global `supervised-indicator`) for
        // an account with no relationship.
        self.has_family = false;
        self.family = crate::family::FamilyState::default();
        // Drop the ward's screen-time state for the same reason, and one more:
        // a lock left standing would accuse a guardian the *next* identity does
        // not have, and would keep a fresh account locked out of every page but
        // Family with no way to appeal (the bug class
        // `critical_alerts::clear_for_identity_change` exists for).
        self.screen_lock = crate::screen_lock::ScreenLock::default();
        // Drop the content-policy render state for the same reason, plus one of
        // its own: the Notify accumulator holds counts not yet reported, and
        // those belong to the departing ward — flushing them under the next
        // identity would attribute one child's flagged content to another.
        self.content_policy = crate::content_policy::ContentPolicyState::default();
        // …but the region source is the DEVICE's, not the departing identity's:
        // re-arm it on the fresh engine, and forget only the session's nest.
        self.region.clear_session();
        crate::region::apply_rule_sets(self);
        // Drop the settings account surface (releases the client Arc + zeroes the
        // identity secret + clears the account-switcher snapshot); the
        // client-local prefs survive.
        self.settings.clear_session();
        // (The critical-alerts registry was already dropped at the top of this
        // function, by `begin_identity_teardown` — it is owed by every teardown,
        // not just this one.)
        //
        // ⚠ `succession_sweep`, `succession_kit_owed` and `succession_predecessor`
        // are deliberately NOT cleared here, and
        // they are the only authenticated-state fields that aren't. Every drop above exists so one identity's state cannot paint
        // under the next one; the succession fields are the inverse — they
        // belong to the *outgoing* identity's ceremony, and this very teardown
        // is that ceremony's closing act. The sweep is its result (rendered
        // after the switch); the owed kit is its last step (performed after the
        // switch, because only the successor's own session can mint it); the
        // predecessor id is what that mint must seal, and also the raising event
        // the succession fold parks for the review raises. See their
        // declarations. Do not "fix" this by adding them to the list.
        //
        // `aftermath` is NOT one of those three — it clears here with the
        // rest. It is not the ceremony's result but the live state of passes
        // that re-run at every sign-in until they report done, so carrying it
        // across a teardown would paint the departing session's verdict over
        // the incoming one's (and, after a successful pass, would keep
        // announcing a re-seal that is already finished).
        self.aftermath = AftermathProgress::default();
        // The review roster is per-identity succession-ledger state — it names people
        // *this* owner has yet to adjudicate — so it drops with everything else
        // above. Carrying it across would flag a member chip in the next
        // identity's group over a question that identity was never asked.
        self.member_reviews.clear();
        // Same reasoning, same plane: the filter marks name rules *this* owner
        // has yet to adjudicate. Carrying them across would mark rows in the
        // next identity's filter list by bare row id — and a filter id is a
        // per-nest autoincrement, so the collision is not even hypothetical.
        self.filter_marks.clear();
        self.ledger_store = None;
        self.errors.clear();
        self.injected_error = None;
        self.injected_warning = None;
        self.injected_info = None;
        self.connection = ConnectionState::Disconnected;
        self.focus = 0;
        self.wizard.reset();
        // Any teardown leaves append mode: a switch (the append's own success path,
        // or a sign-out mid-append) must never keep the wizard surface armed over
        // the new/absent session.
        self.adding_account = false;
    }

    /// Switch the active account to `actor_id` and re-launch as it — the tui twin
    /// of linux's tear-down-and-rebuild switch (`long-term-store.md`
    /// § Multi-account evolution). `confirmed` picks the post-re-auth
    /// `set_active_confirmed` over `set_active`, which refuses a flagged account
    /// with [`fauna_client_accounts::AccountError::ConfirmationRequired`] — the
    /// Stage-2 gate. On success this is [`Self::reset`] **without** the namespace
    /// wipe: the whole authenticated surface is dropped and `launch::start`
    /// re-routes over the now-active account (whose full stored trio resolves the
    /// launch machine to `Online` → `establish`, exactly as a fresh boot of that
    /// account would). Returns the registry error for the caller to surface; a
    /// `ConfirmationRequired` is the gate's backstop, resolved by the caller
    /// before it calls with `confirmed = true`.
    pub fn switch_account(
        &mut self,
        actor_id: &str,
        confirmed: bool,
    ) -> Result<(), fauna_client_accounts::AccountError> {
        crate::session::set_active_account(self, actor_id, confirmed)?;
        self.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        // The next account's launch waits for the outgoing one's stop, so an
        // account switch never briefly runs two accounts' pumps.
        self.after_stops(|app| {
            let tx = app.tx.clone();
            crate::launch::start(app, &tx);
        });
        Ok(())
    }

    /// The account was just locked from the Sessions page: the nest revoked
    /// every token, this app's too, so the shell has nothing left to stand on
    /// and the app returns to its launch surface over the SAME account
    /// (`ui/sessions.md` § User actions — "on success the app leaves the shell
    /// for the locked surface"). [`Self::switch_account`] without the switch:
    /// the credentials and the enrollment stay (the lock removes nobody), and
    /// `launch::start` re-routes exactly as a fresh boot would. Until the
    /// launch paths receive `fauna.auth.account_locked` (`behavior/devices.md`
    /// § The locked state), the launch lands where any refused reconnect does.
    pub fn leave_shell_after_lockout(&mut self) {
        self.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        self.after_stops(|app| {
            let tx = app.tx.clone();
            crate::launch::start(app, &tx);
        });
    }

    /// Apply one backend message.
    pub fn handle_message(&mut self, msg: UiMessage) {
        match msg {
            // The `barrier` self-test's work item. Deliberately trivial: the
            // property under test is *when* it was applied relative to the ack,
            // never what it does.
            #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
            UiMessage::BarrierProbe(token) => self.barrier_probe = Some(token),
            UiMessage::Data(DataMessage::ConnectionState(state)) => {
                self.connection_reports.observe(state.as_wire_word());
                self.connection = state;
            }
            // Cache the viewer's own thresholds for content-policy render
            // enforcement. Fetched at login so it is live before the first feed
            // or thread paint; a guardian floor (if any) composes on top,
            // strictest-wins, inside the shared engine.
            UiMessage::Data(DataMessage::SpamPreferencesLoaded(prefs)) => {
                self.content_policy.set_spam_preferences(Some(&prefs));
            }
            UiMessage::Data(DataMessage::RegionReplies(replies)) => {
                crate::region::apply_replies(self, replies);
            }
            // The aftermath's backup leg reported in. Stored verbatim — this
            // surface renders what the pass says and never re-derives a verdict.
            UiMessage::Data(DataMessage::BackupRegrantProgress(progress)) => {
                self.aftermath.backup_regrant = Some(progress);
            }
            // The aftermath's conversations leg reported in. Stored verbatim
            // for the same reason as the two arms above — in particular the
            // partly-owed arm, which is progress AND unfinished at once.
            UiMessage::Data(DataMessage::MlsResealProgress(progress)) => {
                self.aftermath.mls_reseal = Some(progress);
            }
            // The aftermath's capability-grant leg reported in. Stored verbatim
            // for the same reason as the three arms above.
            UiMessage::Data(DataMessage::GrantRemintProgress(progress)) => {
                self.aftermath.grant_remint = Some(progress);
            }
            // The aftermath's file-corpus leg reported in. Stored verbatim for
            // the same reason as the four arms above; unlike them it is a
            // *level*, re-sent whenever the agent's reading moves, so the last
            // message always wins rather than accumulating.
            UiMessage::Data(DataMessage::CorpusResealProgress(progress)) => {
                self.aftermath.corpus_reseal = Some(progress);
            }
            // The aftermath's mail leg reported in. Stored verbatim for the same
            // reason as the five arms above.
            UiMessage::Data(DataMessage::MailBurnProgress(progress)) => {
                // A settled burn marked rows on the Mail page's own machine —
                // re-read the page's painted copy, which predates it.
                if matches!(progress, fauna_core::progress::Passage::Settled(_)) {
                    self.settings.refold_mail_snapshot();
                }
                self.aftermath.mail_burn = Some(progress);
            }
            // The aftermath's drafts leg reported in. Stored verbatim for the
            // same reason as the six arms above — in particular the partly-owed
            // arm, which on a multi-rail plane is progress AND unfinished at
            // once.
            UiMessage::Data(DataMessage::DraftsResealProgress(progress)) => {
                self.aftermath.drafts_reseal = Some(progress);
            }
            // A fresh read of the open review roster. Replaces wholesale — an
            // empty roster is a real, common answer (nobody flagged, or the
            // owner just worked the last one through), so this arm must never
            // treat "empty" as "nothing to apply".
            UiMessage::Data(DataMessage::MemberReviews(roster)) => {
                self.member_reviews = roster;
            }
            // The filter plane's twin, and wholesale for the same reason: an
            // empty list is the real, common answer once the owner has worked
            // the backlog through.
            UiMessage::Data(DataMessage::FilterMarks(ids)) => {
                self.filter_marks = ids;
            }
            // The socket came back after a drop: re-pull every snapshot surface
            // (feed has no poll; a push dropped across the gap is gone). The
            // spawned refetches land back through `DataMessage::Page` /
            // `FeedChanged`, each of which redraws.
            UiMessage::Data(DataMessage::Reconnected) => {
                self.apply_resync(StaleSurfaces::on_reconnect());
            }
            // A server push arrived on the one authenticated socket: refetch only
            // the surfaces its kind implies (the conversations rail owns its own
            // kinds via the shared receive loop; see `PushEvent::invalidates`).
            UiMessage::Data(DataMessage::Push(event)) => {
                // A sync record landed in a shared/synced set we participate in
                // (file-sync.md § Remote-change nudge) — nudge the agent's
                // resident engine for that set to pull now, off its rescan
                // cadence, so a collaborator's / second device's save
                // materializes in seconds instead of a whole rescan interval.
                // The tui twin of linux `app.rs`'s `PushEvent::SyncChanged`
                // handling; best-effort (`SyncAgentState::pull_set_now`).
                if let PushEvent::SyncChanged(ref p) = event {
                    self.sync_agent
                        .pull_set_now(p.folder.clone(), p.folder_hash.as_ref().map(|h| h.to_vec()));
                    // No account-plane arm here: the store runtime reads the
                    // session's push stream itself (`with_session_wakes` at
                    // `session::establish`; `account-data-plane.md` § The
                    // client-side lifecycle, the pump bullet's wake source
                    // (1)), so a scope-tagged nudge wakes its pump with no app glue.
                    // The per-set device-activity render's live-update half
                    // (`file-sync.md` § Implementation status today — the
                    // whole point of the render is that it updates on this
                    // push, with no manual reload, not just on next expand).
                    // Deliberately narrower than `StaleSurfaces`: this is a
                    // lazily-loaded per-row detail, not a snapshot surface
                    // with no poll backstop, so a blanket boolean would
                    // refresh it even while collapsed or showing a different
                    // set — `device_activity_resync_op`'s own guards cover
                    // the row-match, member-row, and sub-page cases; the
                    // page check below is this call site's own job, mirroring
                    // every other page-scoped gesture in this file.
                    if self.page == Page::Settings
                        && let Some(op) =
                            crate::settings::device_activity_resync_op(&self.settings, p)
                    {
                        self.spawn_page_op(PageOp::Settings(op));
                    }
                }
                self.apply_resync(event.invalidates());
            }
            // The redraw at the top of the loop is the whole response: every
            // wizard page re-reads its snapshot from the machine each frame.
            UiMessage::Data(DataMessage::WizardChanged) => {
                self.clamp_focus();
                self.on_wizard_changed();
            }
            UiMessage::Data(DataMessage::RecoverySelfhostedCommand(command)) => {
                self.wizard.selfhosted_command = Some(command);
            }
            // The read is best-effort and lands late, so the surface may have
            // moved on (a retry succeeded, the user fell through). Only a
            // still-showing transient-retry surface takes the list — reviving a
            // dead surface to hang a button on it would be worse than no button.
            UiMessage::Data(DataMessage::LaunchRecoverBoxes(boxes)) => {
                if let crate::launch::LaunchSurface::TransientRetry { recover_boxes, .. } =
                    &mut self.launch
                {
                    *recover_boxes = boxes;
                    // The new CTA lengthens the surface's element list.
                    self.clamp_focus();
                }
            }
            // The deployment-seed custody leg could not confirm off-box
            // recovery: surface it onto `warning-message` so the admin isn't
            // silently unprotected. Clears on the next nav like every
            // injected line (the tui idiom of linux's transient recovery toast).
            UiMessage::Data(DataMessage::RecoveryCustodyWarning(message)) => {
                self.injected_warning = Some(message);
            }
            // The redraw at the top of the loop is most of the response: the
            // page re-reads the manager's snapshot each frame. The one extra
            // step is firing any embed resolves the *new* snapshot implies —
            // guarded on document shape, so this is idempotent and terminates.
            // The redraw at the top of the loop paints the fresh snapshot; the
            // one extra step is mirroring the manager's error onto the page's
            // `error-message` surface, which `App` owns.
            UiMessage::Data(DataMessage::SearchChanged) => {
                self.clamp_focus();
                crate::search::sync_error(self);
            }
            UiMessage::Data(DataMessage::FeedChanged) => {
                self.clamp_focus();
                crate::feed::fire_resolves(&self.feed);
                // The snapshot may now name a resolved `post-image` hash whose
                // bytes the cache has never fetched — `fire_resolves` folds the
                // hash into the document, this fetches the bytes behind it. The
                // tick is the right trigger (same as Media's thumbnails): it also
                // fires after a compose+submit, whose own new image must paint
                // without re-entering the page. Idempotent — the cache, not this
                // call site, is what makes a repeat fetch impossible.
                if let Some(op) = crate::feed::kick_image_fetches(self) {
                    self.spawn_page_op(PageOp::Feed(op));
                }
                // …and the same for a `doc-remote-image` the reader just
                // revealed: `reveal_remote_images` re-emits the snapshot with
                // `revealed: true`, which is the tick that first offers this url
                // (a blocked image has none — `revealed_remote_image_urls`).
                // Different host, same idempotence: its own cache, not this call
                // site, is what makes a repeat fetch impossible.
                if let Some(op) = crate::feed::kick_remote_image_fetches(self) {
                    self.spawn_page_op(PageOp::Feed(op));
                }
                // …and `c2pa-badge`'s own provenance check for the same
                // resolved `post-image` hash — a separate read of the same
                // blob, on its own idempotent cache (`ui/media.md` § C2PA
                // provenance).
                if let Some(op) = crate::feed::kick_c2pa_fetches(self) {
                    self.spawn_page_op(PageOp::Feed(op));
                }
            }
            // Every page's spawned network half lands here — one arm, because
            // the fold is `apply_page_outcome`'s job, not this match's. The
            // agent's path folds the identical outcome before its reply.
            //
            // Dropped when stale — the generic identity seam (`DataMessage::Page`'s
            // own doc comment): a result already in flight when an actor change
            // landed must not fold into whatever page state the incoming actor's
            // `session::establish` has since installed.
            UiMessage::Data(DataMessage::Page(session_generation, outcome)) => {
                if session_generation == self.session_generation {
                    apply_page_outcome(self, outcome);
                    // The fold may have grown or shrunk the element list (an
                    // error slot appearing, a row list refetched).
                    self.clamp_focus();
                } else {
                    tracing::debug!(
                        "page outcome: dropping a result from session generation \
                         {session_generation} (now {})",
                        self.session_generation
                    );
                }
            }
            // The redraw at the top of the loop is most of the response: the page
            // re-reads the manager's snapshot each frame; re-seat the focus ring
            // in case the thread list grew/shrank. The one extra step mirrors
            // Media: bridge the active compose's `send_state` onto `error-message`
            // (`tui.md` § The page-module contract), so a send failure stamped by
            // ANY path — not just an awaited `Op::SendThread`/`SendNewThread` —
            // surfaces (e.g. `inject_send_failure_for_test`, which mutates the
            // manager directly with no `Op` in flight).
            UiMessage::Data(DataMessage::ConversationsChanged) => {
                crate::conversations::sync_page_error(self);
                self.clamp_focus();
                // A new inbound message in a thread the user is not reading
                // raises a terminal notification — `conversations` outcome 11.
                // The when/for-whom decision is the shared tracker's; this tick
                // is only the edge that feeds it (see `fire_message_banners`).
                crate::conversations::fire_message_banners(&self.conversations);
                // The re-emit after `reveal_remote_images` is the tick that first
                // offers a revealed body image's url — feed's shape, same reason
                // (the cache is the idempotence, so a burst of ticks is one fetch).
                if let Some(op) = crate::conversations::kick_remote_image_fetches(self) {
                    self.spawn_page_op(PageOp::Conversations(op));
                }
                // The rooms the composer offers (`own_rooms`) are a projection of
                // this plane, so its tick is the trigger: a room joined, bound or
                // left reaches the audience select without re-entering the feed.
                // Idempotent — the manager notifies only when the list changed.
                if let Some(feed) = self.feed.manager.clone() {
                    tokio::spawn(async move { feed.refresh_own_rooms().await });
                }
            }
            // Same shape: the page re-reads the machine's snapshot each frame;
            // re-seat the ring in case the item list grew/shrank (a refresh
            // landing, a filter narrowing). The one extra step is bridging the
            // machine's page error onto `error-message`, which is registered off
            // `App::errors` — the machine owns that error, so a failure with no
            // gesture behind it (a background refresh) must still surface.
            UiMessage::Data(DataMessage::MediaChanged) => {
                crate::media::sync_page_error(self);
                self.clamp_focus();
                // The item list just changed, so it may name thumbnails the
                // cache has never loaded. The tick is the right trigger rather
                // than the nav edge: it also fires after an upload, whose new
                // item's thumbnail must paint without re-entering the page.
                // Idempotent — the cache, not this call site, is what makes a
                // repeat fetch impossible, so a burst of ticks costs one fetch.
                if let Some(op) = crate::media::kick_thumbnail_fetches(self) {
                    self.spawn_page_op(PageOp::Media(op));
                }
            }
            // The account-store runtime assembled. Install its handle only if
            // the session it was assembled for is still the live one — a
            // sign-out or account switch while the assembly ran means this
            // handle pumps a retired session's account, so it is shut down
            // instead (the deterministic teardown, not just a drop, so the
            // store thread exits now rather than at process end).
            UiMessage::Data(DataMessage::AccountStoreReady { actor_id, handle }) => {
                let live = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.actor_id == actor_id);
                if live {
                    // The W5.2 change-notice poller rides the handle's
                    // lifetime: it exits when the runtime is gone (sign-out's
                    // deterministic `shutdown`; a plain quit takes the whole
                    // process), so the handle clone it holds never pins a
                    // runtime the app has let go of.
                    tokio::spawn(crate::session::account_store_watch(
                        handle.clone(),
                        self.tx.clone(),
                    ));
                    self.settings.set_account_store(Some(handle.clone()));
                    // The succession ledger's seam, and the post-store-ready
                    // pass over it: the member-item and filter-mark raises a
                    // parked ceremony owes, then the review surfaces' re-read
                    // (`fauna_client_recovery::ledger_aftermath`).
                    self.ledger_store = Some(Arc::new(handle.clone()));
                    crate::session::spawn_ledger_aftermath(self);
                    // The deployment-seed custody leg (`box-recovery.md`
                    // § The plane-era recovery floor, (c) The writes): this
                    // edge normally lands second after the post-auth hook,
                    // so it is the run that captures; later post-auth edges
                    // re-run it (`session::reconverge_post_auth`).
                    if let Some(session) = self.session.as_ref() {
                        crate::recovery::spawn_custody_leg(
                            Arc::clone(&session.client),
                            handle.clone(),
                            self.tx.clone(),
                        );
                    }
                    // Re-read the hide list now that the store is up (the
                    // login-time read may have waited the handle bound out).
                    crate::report::spawn_load_hidden(self);
                    // The Nostr page's npub-confirm check reads the plane
                    // (`fauna.state.nostr-confirmation`); a visit that began
                    // before this edge read no stamp and showed no banner, so
                    // it re-asks now that the store exists.
                    if self.page == Page::Nostr
                        && let Some(op) = crate::nostr::nav_enter_op(self)
                    {
                        self.spawn_page_op(PageOp::Nostr(op));
                    }
                    // The devices page's remove-device action's plane leg
                    // (`devices.md` § Removing a Device) needs the handle too.
                    self.settings.wire_fleet_removal(handle.clone());
                    // The conversations seams that rest on the store: a
                    // community room's wrap keypairs (its nest-side half
                    // registered at login) and the native rail's read
                    // positions — both only exist once the store does.
                    crate::conversations::conv_backend::wire_account_store_seams(
                        &self.conversations,
                        handle.clone(),
                    );
                    // The custody-ceremony drive's crash-recovery edge: owed
                    // ceremony acts (posts, row writes, receipt posts) that a
                    // store-less earlier drive left owed complete now that
                    // the registry door exists (T16; `custody_glue`).
                    self.spawn_custody_drive();
                    // The offline-share seat's ceremony record is lent at
                    // this edge — the panel may have bound long before it
                    // (`p2p.md` § Offline share initiation → *The seat's
                    // record is lent late*).
                    #[cfg(feature = "p2p-share")]
                    crate::offline_share::lend_account_record(
                        &self.settings.offline_share.session_seat,
                        handle.clone(),
                    );
                    // The share plane rides the same edge: every seam it
                    // pumps through (cached brake, dial-target cache, the
                    // sink's durable write, the transfer ledger) lives
                    // behind this handle (`share_glue` module docs).
                    #[cfg(feature = "p2p-share")]
                    self.spawn_share_glue(handle);
                } else {
                    // Queued behind any stop in flight, never raced against it.
                    // The teardown that retired this session took its
                    // `PendingAssembly` and is stopping THIS runtime under its
                    // own reason — a sign-out retiring the machine's enrollment
                    // first. A plain `shutdown` landing ahead of that would
                    // leave the retirement reading `account runtime is shut
                    // down`: one stranded device row per mid-assembly sign-out
                    // (`account-runtime.md` § Implementation status today →
                    // the superseded shutdown). Once the stop has run this is
                    // a re-shut of a stopped handle, harmless.
                    self.after_stops(move |_| {
                        tokio::spawn(async move { handle.shutdown().await });
                    });
                }
            }
            UiMessage::Data(DataMessage::AccountRuntimeStopped) => {
                self.stops.stop_finished();
                self.run_ready_after_stops();
            }
            // Repaint-only, both: the redraw at the top of the loop re-reads
            // the share plane's state cell, and the seat the glue bound is
            // already the panel's (`SharePlaneSeatBound`'s docs).
            #[cfg(feature = "p2p-share")]
            UiMessage::Data(DataMessage::SharePlaneSeatBound | DataMessage::SharePlaneChanged) => {}
            // Ceremony state advanced on the conversations poll path (an
            // offer/accept/deliver/receipt was captured): run the
            // record-then-act loop's "act" half in the background.
            UiMessage::Data(DataMessage::CustodyCeremonyMoved) => {
                self.spawn_custody_drive();
            }
            // A sibling same-account process committed to the shared store:
            // re-drive the open surface's own load (reload semantics — the
            // same refetch a same-page nav fires). Exactly two surfaces render
            // store-backed snapshots today: the store-reading Settings
            // sub-pages, and the feed's sealed scorers (muted keywords /
            // trained factors), which load only inside the feed's reload.
            UiMessage::Data(DataMessage::AccountStoreChanged) => {
                // The reporter-side hide list is a render source on every
                // page (like the spam thresholds), not a page's own data, so
                // it re-reads whatever page is open — a sibling may have
                // reported something.
                crate::report::spawn_load_hidden(self);
                match self.page {
                    Page::Settings => {
                        if let Some(op) = crate::settings::store_resync_op(&mut self.settings) {
                            self.spawn_page_op(PageOp::Settings(op));
                        }
                    }
                    Page::Feed => {
                        if let Some(op) = crate::feed::nav_enter_op(&self.feed) {
                            self.spawn_page_op(PageOp::Feed(op));
                        }
                    }
                    _ => {}
                }
            }
            // The sync agent came back (its convergence loop's reachable edge):
            // re-drive the folder-binding reconcile against the agent's
            // `ListLocations` truth. Never only once at attach — the spawned task
            // adopts agent-side rows and re-pushes any pending binding, then posts
            // `SyncAgentChanged` if the rendered set moved.
            UiMessage::Data(DataMessage::SyncAgentReconcile) => {
                let tx = self.tx.clone();
                self.sync_agent.drive_reconcile(&tx);
            }
            // The reconcile changed the rendered folder-binding set: the redraw at
            // the top of the loop re-reads it (Slice 3's Folders binding section).
            // Re-seat the focus ring in case the row list grew/shrank.
            UiMessage::Data(DataMessage::SyncAgentChanged) => {
                self.clamp_focus();
            }
            // Same page-scoping as the device-activity resync above; the op's
            // own guards cover the row match.
            UiMessage::Data(DataMessage::FolderBindConfirmed(set)) => {
                if self.page == Page::Settings
                    && let Some(op) = crate::settings::roster_resync_op(&self.settings, &set)
                {
                    self.spawn_page_op(PageOp::Settings(op));
                }
            }
            // The agent's health reading moved: nothing to fold — the redraw at
            // the top of the loop re-reads it for the sidebar's
            // `sync-agent-status` line. It is shell chrome, not a focusable
            // element, so the focus ring is untouched.
            UiMessage::Data(DataMessage::SyncAgentStatusChanged) => {}
            UiMessage::Data(DataMessage::LaunchPhase(snapshot)) => {
                let tx = self.tx.clone();
                crate::launch::route(self, &tx, snapshot);
                // The new surface has a different element count.
                self.focus = 0;
            }
            UiMessage::Data(DataMessage::LaunchMachineChanged) => {
                let tx = self.tx.clone();
                crate::locked::follow_snapshot(self, &tx);
            }
            UiMessage::Data(DataMessage::LockedCeremony(outcome)) => {
                crate::locked::fold_stolen(self, *outcome);
                self.focus = 0;
            }
            UiMessage::Data(DataMessage::SelfAddressRefreshed) => {
                // Push the registry's just-landed handle/domain into the
                // conversations session's live self-address cell — the ONE
                // self-heal call (`conversations.md` § State & data shape →
                // *Self-address: live, never baked*): the next SMTP send
                // carries the resolved `From:` (a pre-resolution session stops
                // refusing `no_handle`), MLS routes same-nest peers against the
                // resolved domain, and reply-all drops the current self. No-op
                // until an address actually resolves — never push a partial
                // (the empty cell keeps the honest local-refusal floor).
                if let Some(conv) = self.conversations.real_session.clone()
                    && let Some(addr) = crate::session::session_self_address(self)
                {
                    conv.set_self_address(addr);
                }
            }
            UiMessage::Data(DataMessage::NestIdentityChanged) => {
                // `security.md` § Post-auth surfacing: route the verdict to the
                // SAME blocking launch surface the launch path renders. NOT the
                // error/toast path — that is for faults a session can survive,
                // and this session cannot (its connections can no longer
                // graduate), so a soft surface over it would be a generic-error
                // mystery instead of the honest verdict.
                //
                // Tear down and **re-enter the real launch flow** rather than
                // setting `LaunchSurface::IdentityChanged` in place: the
                // re-trust button drives `trust_nest_identity()` on the machine
                // that produced the verdict (`launch.rs`'s `LaunchAction::Trust`
                // self-guards to `IdentityChanged`), so a synthesized surface
                // would render identically with a button that silently does
                // nothing. Re-running the challenge costs one round trip and
                // yields a LIVE machine.
                //
                // Credentials are deliberately KEPT (`sign_out`, not `reset`):
                // nothing is wrong with the identity — the *nest* changed — and
                // both exits (re-trust, use a different nest) need it in hand.
                //
                // ⚠ This arm is REACHABLE more than once per process: the
                // surface's Trust action re-`route`s into `session::establish`,
                // which spawns a fresh alert-sweep loop. So the teardown
                // bookkeeping inside `escalate_to_launch_surface` is what stops
                // the departing loop — without it, one leaks per re-trust.
                self.escalate_to_launch_surface();
            }
            UiMessage::Data(DataMessage::IdentitySuperseded) => {
                // A mid-session supersession (`identity-succession.md`
                // § Propagation → *Own device fleet*). Same escalation shape as
                // `NestIdentityChanged` directly above, and for the same reason:
                // the session cannot survive it, so a toast would be a mystery
                // where the launch surface is the honest answer.
                //
                // Re-entering the real launch flow (rather than synthesizing a
                // surface) is what makes the affordance correct here too: the
                // re-run challenge earns the same refusal, the machine parks in
                // its terminal `Superseded` state, and `launch::route` lands the
                // user on the identity-import screen with the explanation. One
                // round trip, and one code path shared with a cold start.
                //
                // Credentials are KEPT (`sign_out`, not `reset`): the old secret
                // is still the user's, and it is what a successor ceremony and
                // any later re-import reason about. The account moved, not the
                // person.
                //
                // Except when this device's own stolen-identity ceremony caused
                // it: then the ceremony's fold decides (adopt and switch, or
                // park the key it could not store), and escalating first would
                // drop that fold as stale.
                if !self.defer_own_supersession() {
                    self.escalate_to_launch_surface();
                }
            }
            UiMessage::Data(DataMessage::SignInRefused) => {
                // Suspended or removed while signed in. Same escalation, same
                // reason: every connection this session opens is now refused,
                // so a banner would sit over a dead session. The re-run
                // challenge meets the same refusal and `launch::route` lands the
                // refused surface, whose Retry drives the new live machine —
                // the admin's restore is the way back in. Credentials KEPT: the
                // identity is still the user's.
                self.escalate_to_launch_surface();
            }
            UiMessage::Data(DataMessage::AccountLocked) => {
                // Locked while signed in. Same escalation, same reason: the
                // lock revoked every bearer and the nest refuses to mint
                // another, so a banner would sit over a dead session. The
                // re-run challenge meets the same refusal and `launch::route`
                // lands the locked surface on a live machine — the one whose
                // refresh at the unlock time brings the app back. Credentials
                // KEPT: the stolen-identity ceremony needs the seed.
                self.escalate_to_launch_surface();
            }
            UiMessage::Data(DataMessage::IdentitySupersededVerified {
                successor,
                predecessor,
            }) => {
                // The chain proved it, so the screen may now say who holds the
                // account. Only ever an upgrade of the *message*: the user is
                // already on the import screen, and this must not move them —
                // save in the one case where there is nothing left to import,
                // because this device already holds the proven successor's key.
                //
                // Guarded on still being on that screen — the verify is a
                // best-effort round trip that can land after the user navigated
                // away (or typed their seed and left), and re-asserting a stale
                // supersession over whatever they are now doing would be a
                // mystery banner from a flow they already dealt with.
                if self.wizard.machine.step()
                    == fauna_onboarding_machine::OnboardingStep::IdentityImport
                    && !self.adopt_held_successor(&predecessor, &successor)
                {
                    self.wizard.machine.begin_import_identity_with_reason(
                        fauna_i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED_VERIFIED
                            .replace("{successor}", &successor),
                    );
                }
            }
            UiMessage::Noop => {}
        }
    }

    /// Fire the reads a wizard page needs **on entry**, once per visit.
    ///
    /// The observer ticks on every machine mutation, so the trigger is the step
    /// *edge*, not the tick: a per-tick network read would hammer the surviving
    /// nest (and `set_recovery_boxes` itself ticks, which would loop).
    ///
    /// Both reads are best-effort and both are `spawn`ed: the render loop must
    /// never block on a nest that is — very plausibly, on this of all flows —
    /// unreachable.
    fn on_wizard_changed(&mut self) {
        use fauna_onboarding_machine::OnboardingStep;

        let step = self.wizard.machine.step();
        if self.last_wizard_step == Some(step) {
            return;
        }
        self.last_wizard_step = Some(step);

        match step {
            OnboardingStep::NestRecovery => self.fetch_recovery_boxes(),
            OnboardingStep::RecoverSelfhostedInstructions => self.fetch_selfhosted_command(),
            _ => {}
        }
    }

    /// The `nest_recovery` box list: this device's own account store joined
    /// with a cold read from the wizard's nest when one is known — ONE resolver
    /// call, never either-or (`box-recovery.md` § The plane-era recovery floor,
    /// (b) The reads).
    ///
    /// The result is pushed into the machine **only when non-empty**, so a failed
    /// or empty read never clobbers a list already on screen. That is not just
    /// defensive: the tier_2 suite injects the list via `set_recovery_boxes` and
    /// *then* flips the step, so an unguarded empty push would race the injection
    /// and blank the page out from under the test.
    fn fetch_recovery_boxes(&self) {
        let Some(secret) = self.wizard.machine.effective_secret() else {
            return; // No identity yet ⇒ no account to read custody for.
        };
        let nest_url = Some(self.wizard.machine.nest_url()).filter(|u| !u.is_empty());
        let machine = std::sync::Arc::clone(&self.wizard.machine);
        tokio::spawn(async move {
            let boxes = crate::recovery::load_recoverable_boxes(nest_url.as_deref(), &secret).await;
            if !boxes.is_empty() {
                machine.set_recovery_boxes(boxes);
            }
        });
    }

    /// The `recover-selfhosted-command` for the selected box. Until it lands, the
    /// page shows the pending placeholder — never another box's command.
    fn fetch_selfhosted_command(&self) {
        let Some(secret) = self.wizard.machine.effective_secret() else {
            return;
        };
        let Some(box_id) = self.wizard.machine.recovery_selected_nest_id() else {
            return; // Unreachable: the method buttons are gated on a selection.
        };
        let nest_url = Some(self.wizard.machine.nest_url()).filter(|u| !u.is_empty());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Some(command) =
                crate::recovery::load_selfhosted_command(nest_url.as_deref(), &secret, &box_id)
                    .await
            {
                let _ = tx.send(UiMessage::Data(DataMessage::RecoverySelfhostedCommand(
                    command,
                )));
            }
        });
    }

    /// Apply one key press — **one keymap for every screen**.
    ///
    /// The authenticated shell used to run a parallel interaction model (↑/↓
    /// bound straight to page selection, bare `q` to quit). That cannot host a
    /// page with inputs: `q` and every other letter would collide with the
    /// composer, and ↑/↓ cannot both switch pages and move within a feed. Worse,
    /// no e2e would ever catch it — the automation agent actuates elements
    /// directly, so the client could ship green and keyboard-dead for a human.
    /// So both screens now drive the same [`Element`](crate::element::Element)
    /// list:
    ///
    /// - **↑/↓/Tab/BackTab** move the ring within the active zone.
    /// - **←/→** switch zone (sidebar ⇄ page); authenticated only.
    /// - **Enter** activates the focused element.
    /// - **Printable keys** type into a focused [`Role::Input`]. Only when no
    ///   input holds focus do they act as commands (`q` quits, vi `j`/`k`
    ///   move) — so `q` can never quit out from under someone typing.
    /// - **Ctrl+C** always quits.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        // Sampled *before* the key is applied, so the load-more trigger below
        // fires on the ring's arrival at the last card — the EDGE — and not once
        // per keystroke for as long as it rests there. (Same discipline as the
        // wizard's page-entry reads, which fire on the step edge, not the tick.)
        let was_at_last_card = crate::feed::at_last_card(self);
        match key.code {
            // Abandon an in-flight append-mode "Add account" and return to the live
            // session — the guaranteed in-app escape (`long-term-store.md`
            // § Multi-account evolution). HIGHEST-priority Esc arm: the underlying
            // session's page state (e.g. `settings.sub == Account`) is untouched
            // beneath the wizard, so a lower Esc arm would otherwise wrongly consume
            // this as a page-back instead of leaving append mode.
            KeyCode::Esc if self.adding_account => {
                self.abandon_add_account();
            }
            // The feed's two modal overlays close FIRST, innermost outward, and
            // before the sub-page arm below — Esc over an open lightbox must
            // shut the lightbox, not leave `post_detail` behind it. ui.yaml
            // declares a dismiss element inside neither `image-lightbox` nor
            // `feed-compose-dialog`, so this is the human-only affordance, the
            // same shape as the conversations detail overlays (the agent
            // re-opens via a fresh `reset`).
            KeyCode::Esc if self.feed.lightbox.is_some() => {
                self.feed.lightbox = None;
            }
            // The tip attribution window is the same class — a modal overlay
            // ui.yaml gives no dismiss element of its own, so Esc is the
            // human-only affordance and the agent re-opens via a fresh `reset`.
            #[cfg(feature = "payments")]
            KeyCode::Esc if self.feed.tip_list_open.is_some() => {
                self.feed.tip_list_open = None;
            }
            KeyCode::Esc if self.feed.compose_dialog_open => {
                self.feed.compose_dialog_open = false;
            }
            // The reply dialog is the same class as the two above: ui.yaml
            // registers no dismiss element for `feed-reply-dialog`, so Esc is
            // the human-only affordance (the agent closes one it opened via a
            // fresh `reset`).
            KeyCode::Esc if self.feed.reply_draft.is_some() => {
                self.feed.reply_draft = None;
            }
            // Leave a feed sub-page (`create_feed` / `post_detail`) back to the
            // list. ui.yaml gives `post_detail` no dismiss element of its own (a
            // human-only affordance — the agent never opens one it doesn't also
            // close via a fresh `reset`), so this is a keymap-only escape, not a
            // registered element.
            KeyCode::Esc if self.feed.mode != crate::feed::Mode::List => {
                self.feed.mode = crate::feed::Mode::List;
            }
            // A detail-view modal overlay (add-participant / rename) is dismissed
            // FIRST — Esc cancels the overlay and stays on the thread, matching the
            // GUI apps' modal dialogs (linux `add_participant_overlay` /
            // `rename_overlay`, closed on the dialog's Cancel). A second Esc then
            // leaves the thread. ui.yaml gives the overlays no dismiss element, so
            // this is the human-only back affordance (the agent cancels an overlay
            // it opened via a fresh `reset`).
            KeyCode::Esc if crate::conversations::detail_overlay_open(&self.conversations) => {
                crate::conversations::cancel_detail_overlay(&mut self.conversations);
            }
            // The same keymap-only escape for the conversations sub-pages (the
            // `compose` and `conversation_detail` panes) — ui.yaml gives neither a
            // dismiss element of its own, so Esc is the human-only back affordance
            // (the agent closes a sub-page it opened via a fresh `reset`).
            KeyCode::Esc if self.conversations.mode != crate::conversations::Mode::List => {
                crate::conversations::show_list(&mut self.conversations);
            }
            // Leave the Address Book's `card_detail` sub-page back to the card
            // list. Same keymap-only shape as the feed/conversations arms above:
            // ui.yaml's `card_detail` declares no dismiss element (the agent
            // re-opens via a fresh `reset`), so Esc is the human's way back.
            KeyCode::Esc if self.contacts.open_card.is_some() => {
                self.contacts.open_card = None;
            }
            // Leave a settings sub-page back to the rail Root. Unlike the two above,
            // this sub-page DOES carry a registered dismiss element
            // (`settings-nav-back`), so this Esc is a redundant human convenience,
            // not the agent's only way out.
            // A pending Stage-2 re-auth prompt eats Escape as a decline (a pure
            // no-op) so it never falls through to the nav-back below
            // (`long-term-store.md` § Per-account re-auth: cancel/Escape both take
            // the decline path).
            KeyCode::Esc
                if self.settings.sub == crate::settings::SubPage::Account
                    && self.settings.reauth_prompt_open() =>
            {
                self.settings.cancel_reauth_prompt();
            }
            // Abandon an open folder creation wizard, staying ON the File
            // sets list rather than leaving Settings entirely. ui.yaml gives
            // the wizard no cancel button of its own (only `wizard-next/back/
            // create-button` — a human-only escape, matching the feed/
            // conversations sub-page Esc arms above), so this is the ONLY way
            // to abandon a wizard short of creating it.
            KeyCode::Esc
                if self.settings.sub == crate::settings::SubPage::Folders
                    && self.settings.folders_wizard_open() =>
            {
                self.settings.close_folders_wizard();
            }
            KeyCode::Esc if self.settings.sub != crate::settings::SubPage::Root => {
                self.settings.sub = crate::settings::SubPage::Root;
            }
            KeyCode::Tab | KeyCode::Down => self.focus_next(),
            KeyCode::BackTab | KeyCode::Up => self.focus_prev(),
            // Pane switching. The page pane's ring restarts at the top: it may
            // have been left on an element the new page doesn't have.
            KeyCode::Left if self.authenticated() => self.enter_sidebar_zone(),
            KeyCode::Right if self.authenticated() => self.enter_page_zone(),
            // A disabled button is inert here exactly as it is for the agent's
            // click — one gate, both paths.
            KeyCode::Enter => self.actuate_focused(),
            KeyCode::Char(c) => match self.focused_input() {
                Some(field) => {
                    let mut text = self.field(field.clone());
                    text.push(c);
                    self.spawn_field_write(field, text);
                }
                // No input has focus, so a letter is a command, not text.
                None => match c {
                    'q' => self.should_quit = true,
                    'k' => self.focus_prev(),
                    'j' => self.focus_next(),
                    _ => {}
                },
            },
            KeyCode::Backspace => {
                if let Some(field) = self.focused_input() {
                    let mut text = self.field(field.clone());
                    text.pop();
                    self.spawn_field_write(field, text);
                }
            }
            _ => {}
        }

        // Reaching the bottom of the feed asks for the next page — the TUI's
        // equivalent of a GUI app's scroll-edge trigger. `has_more` gates it,
        // and the edge check above keeps it to one request per arrival.
        if !was_at_last_card && crate::feed::at_last_card(self) {
            self.spawn_gesture(crate::element::Gesture::Feed(crate::feed::Action::LoadMore));
        }
        // The nav-edge hook's other ordinary call site (`gesture_work`'s doc
        // comment names the first): catches the Esc arms above, which mutate
        // `self.settings.sub` directly and never reach `gesture_work` at all.
        // Redundant-but-harmless for a key whose arm already ran through
        // `gesture_work` via `spawn_gesture`/`actuate_focused` above.
        self.sync_recovery_message_nav_edge();
    }

    /// Hand the keyboard to the page pane, ring at the top.
    pub fn enter_page_zone(&mut self) {
        self.zone = Zone::Page;
        self.focus = 0;
        self.clamp_focus();
    }

    /// Hand the keyboard back to the sidebar — the `KeyCode::Left` twin of
    /// [`Self::enter_page_zone`]. No focus reset: in the sidebar the ring
    /// *is* [`Self::page`], which needs no re-seating.
    pub fn enter_sidebar_zone(&mut self) {
        self.zone = Zone::Sidebar;
    }

    /// Actuate whatever the focus ring currently sits on — the keyboard's
    /// `Enter`, factored out so a real terminal mouse click (which moves the
    /// ring to the clicked element first, [`Self::click_sidebar`] /
    /// [`Self::click_page_element`]) fires through the exact same dispatch
    /// rather than a second, driftable copy of it (the lesson [`gesture_work`]'s
    /// own doc comment already states, one level up: a hand-written second
    /// table is how an actuation silently no-ops on a role it forgot to list).
    fn actuate_focused(&mut self) {
        use crate::element::Role;
        match self.focused() {
            Some(e) if !e.enabled => {}
            Some(e) => match e.role {
                // A sidebar tab commits the selection and steps into the page —
                // the row is already selected (the ring *is* the selection), so
                // the useful gesture is "go there".
                Role::Button(crate::element::Gesture::Nav(page)) => {
                    // Through the one door, so the nav-edge refresh spawns like
                    // every other keyboard-path op.
                    self.spawn_gesture(crate::element::Gesture::Nav(page));
                    self.enter_page_zone();
                }
                Role::Button(gesture)
                | Role::Checkbox { gesture, .. }
                | Role::Radio { gesture, .. } => {
                    self.spawn_gesture(gesture);
                }
                // The reply To-line's add-input is a type-and-add field
                // (linux's gtk::Entry activates on Enter): Enter commits the
                // typed recipient as a chip, rather than advancing. It is an
                // input whose Enter is a submit, so it is special-cased here
                // rather than in the generic advance below. (The ⋯ menu's
                // free-entry emoji prompt used to sit beside it and now paints
                // as `Role::InputCommit`, so the generic arm below carries it —
                // which is also what makes it drivable by the automation
                // surface at all.)
                Role::Input(crate::element::Field::Conversations(
                    crate::conversations::ConversationsField::ReplyRecipientAdd,
                )) => self.spawn_gesture(crate::element::Gesture::Conversations(
                    crate::conversations::Action::CommitReplyRecipientAdd,
                )),
                // Enter in a passphrase input submits — the terminal convention
                // for a password prompt (and the same special-cased submit shape
                // as the one above).
                Role::Input(crate::element::Field::Unlock(
                    crate::unlock::UnlockField::Passphrase | crate::unlock::UnlockField::Confirm,
                )) => self.spawn_gesture(crate::element::Gesture::Unlock(
                    crate::unlock::UnlockAction::Submit,
                )),
                // A committing input IS the "Enter submits" case, declared by the
                // page rather than special-cased above: fire its gesture. This is
                // the generic form of the two hand-listed inputs above.
                Role::InputCommit { gesture, .. } => self.spawn_gesture(gesture),
                // Enter on an input advances, matching every form on the other
                // six apps (submit is the next focusable).
                Role::Input(_) => self.focus_next(),
                // A terminal has no dropdown to pop open, so Enter cycles to the
                // next option — the keyboard equivalent of picking one.
                Role::Select {
                    target, options, ..
                } => {
                    if let Some(next) = options
                        .iter()
                        .position(|o| *o == e.text)
                        .map(|i| (i + 1) % options.len())
                        .and_then(|i| options.get(i))
                    {
                        self.spawn_gesture(target.gesture(next.clone()));
                    }
                }
                Role::Label => {}
            },
            None => {}
        }
    }

    /// Move the focus ring onto the sidebar row at `index` (a position in
    /// [`Self::sidebar_pages`]) and actuate it — real-terminal mouse click
    /// support (`apps/tui.md` § Architecture: "mouse support where the
    /// terminal offers it"). An out-of-range index (a stale hit-test against a
    /// frame that has since scrolled) is a no-op.
    pub fn click_sidebar(&mut self, index: usize) {
        if let Some(&page) = self.sidebar_pages().get(index) {
            self.zone = Zone::Sidebar;
            self.set_page(page);
            self.actuate_focused();
        }
    }

    /// Move the focus ring onto the page element at `index` (a position in
    /// [`Self::page_elements`], not the focusable-only ring index) and actuate
    /// it, mirroring [`Self::click_sidebar`]. A non-focusable hit (a label) is a
    /// no-op — the same as the keyboard ring, which can never land there either.
    pub fn click_page_element(&mut self, index: usize) {
        let elements = self.page_elements();
        let Some(element) = elements.get(index) else {
            return;
        };
        if !element.focusable() {
            return;
        }
        let focus = elements[..index].iter().filter(|e| e.focusable()).count();
        self.zone = Zone::Page;
        self.focus = focus;
        self.actuate_focused();
    }

    /// A **second** press at the same target: move the ring exactly as
    /// [`Self::click_page_element`] does, then spawn the element's second
    /// gesture ([`Element::dbl`](crate::element::Element::dbl)) instead of its
    /// first — the month day cell's "open the new-event compose prefilled with
    /// this date" (`ui/events.md` § Layout & flow).
    ///
    /// Reports whether the element actually had one, so the caller can fall
    /// back to a plain click. A double press on an ordinary button is then
    /// simply the two clicks it is, rather than being swallowed by a
    /// double-press the element never defined.
    pub fn double_click_page_element(&mut self, index: usize) -> bool {
        let elements = self.page_elements();
        let Some(element) = elements.get(index) else {
            return false;
        };
        let Some(gesture) = element.dbl.clone() else {
            return false;
        };
        if !element.focusable() || !element.enabled {
            return false;
        }
        let focus = elements[..index].iter().filter(|e| e.focusable()).count();
        self.zone = Zone::Page;
        self.focus = focus;
        self.spawn_gesture(gesture);
        true
    }

    /// Fire-and-forget a gesture from the keyboard path, so a slow network
    /// probe never freezes the render loop.
    ///
    /// This is the **spawn** half of the actuation duality; the agent's path
    /// ([`crate::automation::run_gesture`]) is the **await** half. Both call
    /// [`gesture_work`] — the one door — so neither re-derives which page a
    /// gesture belongs to. All this fn decides is how to run what comes back.
    pub fn spawn_gesture(&mut self, gesture: crate::element::Gesture) {
        match gesture_work(self, gesture) {
            GestureWork::None => {}
            GestureWork::Page(op) => self.spawn_page_op(*op),
            GestureWork::Wizard(machine, action, payload) => {
                let sink = crate::session::confirm_identity_sink(self);
                crate::wizard::spawn_action(machine, sink, action, payload);
            }
            GestureWork::Launch(action) => {
                let tx = self.tx.clone();
                crate::launch::spawn_launch_action(self, &tx, action);
            }
            GestureWork::Retire(work) => {
                tokio::spawn(work.run());
            }
        }
    }

    /// Run a [`PageOp`] off the render loop, its `Outcome` riding the channel
    /// back as one [`DataMessage::Page`] for [`Self::apply_page_outcome`] to fold.
    ///
    /// The **spawn** half of the actuation duality, factored out of
    /// [`Self::spawn_gesture`] because a gesture is not the only thing that
    /// produces page work: a machine's observer tick can too (Media's thumbnail
    /// loads). Both go through this one door, so neither re-derives how an
    /// `Outcome` gets home.
    pub fn spawn_page_op(&self, op: PageOp) {
        let tx = self.tx.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let outcome = op.run().await;
            let _ = tx.send(UiMessage::Data(DataMessage::Page(generation, outcome)));
        });
    }

    /// Spawn the refetches a [`StaleSurfaces`] asks for — the shared body behind
    /// both the
    /// reconnect re-pull and the per-push refresh. Each surface goes through its
    /// existing nav-edge op (so a background refresh reuses the exact fetch a
    /// nav-to-tab would run) and rides home on the observer/`DataMessage::Page`
    /// channel, redrawing when it lands. A surface with no live client yet
    /// (pre-login) yields no op and is skipped. The feed is observer-backed rather
    /// than op-shaped: `refresh_feeds` reloads the feed list and
    /// `refresh_current_feed` reloads the current selection's posts, and the
    /// `FeedManager`'s observer ticks the redraw.
    ///
    /// The re-pull goes through the shared `refresh_current_feed` seam, **never**
    /// `select_feed(snapshot().selected_feed)`: `selected_feed` is `None` both for
    /// the local feed *and* while the built-in Trending virtual feed is selected,
    /// so re-selecting it resolves to Local and silently drops a Trending viewer
    /// on every reconnect/push resync (`trending.md` § The Trending feed; the
    /// shared seam's own doc names this trap, and android/apple/windows each
    /// shipped it before fixing it).
    fn apply_resync(&self, r: StaleSurfaces) {
        if r.feed
            && let Some(m) = self.feed.manager.clone()
        {
            tokio::spawn(async move {
                m.refresh_feeds().await;
                m.refresh_current_feed().await;
            });
        }
        if r.notifications
            && let Some(op) = crate::notifications::nav_enter_op(&self.notifications)
        {
            self.spawn_page_op(PageOp::Notifications(op));
        }
        if contacts_stale(&r)
            && let Some(op) = crate::contacts::nav_enter_op(&self.contacts)
        {
            self.spawn_page_op(PageOp::Contacts(op));
        }
        if r.account
            && let Some(op) = crate::settings::nav_enter_op(&self.settings)
        {
            self.spawn_page_op(PageOp::Settings(op));
        }
        // Unconditional on the page being *shown*: the machine's snapshot is
        // what every later paint reads, and a consent request is minutes-lived,
        // so refreshing only on a visit would mean the card the user navigated
        // to was already stale on arrival.
        if r.atproto
            && let Some(op) = crate::settings::atproto_resync_op(&self.settings)
        {
            self.spawn_page_op(PageOp::Settings(op));
        }
        // The consent card lives on Connected apps now, so the same push
        // re-reads its tray — same reasoning as the line above.
        if r.atproto
            && let Some(op) = crate::settings::connected_apps_resync_op(&self.settings)
        {
            self.spawn_page_op(PageOp::Settings(op));
        }
        if r.events
            && let Some(op) =
                crate::events::nav_enter_op(&self.events, self.settings.account_store.clone())
        {
            self.spawn_page_op(PageOp::Events(op));
        }
        // Page-gated like media below — see `StaleSurfaces::address_book`: a
        // contacts app's first sync is one push per card, and entering the
        // Address Book re-reads it anyway. What the flag buys is the card that
        // lands while the user is looking at the open book.
        if r.address_book
            && self.page == Page::Contacts
            && let Some(op) = crate::contacts::address_book_resync_op(&self.contacts)
        {
            self.spawn_page_op(PageOp::Contacts(op));
        }
        // Page-gated like address_book — see `StaleSurfaces::mail_spam`: a third-
        // party mail app's batch Junk move is one push per lesson, and a visit
        // re-reads the list anyway. What the flag buys is the undo or reset made
        // on another device landing in the list open here.
        if r.mail_spam
            && self.page == Page::Settings
            && let Some(op) = crate::settings::mail_spam_resync_op(&self.settings)
        {
            self.spawn_page_op(PageOp::Settings(op));
        }
        // Page-gated, unlike every arm above — see `StaleSurfaces::media`. `Media` is
        // a cross-*set* aggregate whose nav-enter read already covers the
        // arrive-while-elsewhere case, so the flag buys exactly one thing: the
        // file that lands while the user is looking at the page.
        if r.media
            && self.page == Page::Media
            && let Some(op) = crate::media::nav_enter_op(&self.media)
        {
            self.spawn_page_op(PageOp::Media(op));
        }
        // The ward's supervision read — the same `fauna.family.status` that
        // `session::establish` fires post-auth, so a guardian's edit binds at WS
        // reconnect and not only at the ward's next login
        // (family-client-enforcement.md § Content policy, clause 1: "refresh
        // fires at cold launch and on WS reconnect"). Safe to re-fire: a failed
        // read is `Outcome::Failed`, which moves only the page error, never the
        // floor. Until 2026-09-13 this arm did not exist and tui re-read only at
        // login and on a Family-page visit.
        if r.family {
            crate::family::spawn_status_check(&self.family, &self.tx, self.session_generation);
        }
    }

    /// A keyboard write into a field, spawning any re-query it implies so a slow
    /// nest never freezes the render loop.
    fn spawn_field_write(&mut self, field: crate::element::Field, value: String) {
        if let Some(pending) = self.set_field(field, value) {
            tokio::spawn(pending.run());
        }
    }

    /// Which editable field the ring currently sits on, if any.
    ///
    /// `pub(crate)` because the key-hint footer ([`crate::ui::nav_key_hints_text`])
    /// must answer "would `q` quit, or type a literal `q`?" from the **same**
    /// predicate the `KeyCode::Char` arm above branches on. A second copy of this
    /// test living in `ui.rs` is precisely the hand-written second table this
    /// module's [`Self::actuate_focused`] doc warns about — it would drift, and
    /// the footer would start advertising a key that does something else.
    pub(crate) fn focused_input(&self) -> Option<crate::element::Field> {
        let element = self.focused()?;
        // A DISABLED input takes no keystrokes — the `KeyCode::Char` half of the
        // rule [`Self::actuate_focused`] already applies to `Enter`, so that
        // arm's "one gate, both paths" is now true of both keys rather than
        // only the one it was written beside (`e2e-conventions.md` convention
        // 11: *"typing into a disabled field is the same illegal act"*).
        //
        // The ring still LANDS here — [`Element::focusable`] is deliberately
        // `enabled`-blind, so a reader can tab to a greyed control and read its
        // reason label. What changes is only what a keystroke then does: with
        // no field to write, `Char` falls through to the command keys, which is
        // the same answer the footer gives (this predicate is what
        // [`crate::ui::nav_key_hints_text`] asks, so hint and behaviour cannot
        // disagree).
        if !element.enabled {
            return None;
        }
        match element.role {
            // Both editable roles accept keystrokes — a committing input is an
            // input first; omitting it here would make it un-typeable.
            crate::element::Role::Input(field)
            | crate::element::Role::InputCommit { field, .. } => Some(field),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::element::{Field, Gesture};
    use crate::wizard::WizardField;
    use fauna_ui_ids as ids;

    /// A bare keypress. `pub(crate)` so a page module can pin its own Escape
    /// arm from its own tests — the keymap arms are per-page (each dismisses
    /// that page's overlay), so the assertion belongs beside the page.
    pub(crate) fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A private, file-backed credential namespace for one unit test.
    ///
    /// **Never** the env-resolved store: `App::reset()` clears its namespace
    /// wholesale, and `session::secret_store()` with no `FAUNA_KEYRING_APP` /
    /// `FAUNA_E2E_CREDENTIAL_DIR` set — which is every `cargo test` run —
    /// resolves to the developer's real `fauna-tui` libsecret namespace. A test
    /// touching a real keyring is a bug even when it only reads.
    pub(crate) fn test_credentials() -> Arc<CredentialStore> {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fauna-tui-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(CredentialStore::with_file_backend("fauna-tui-test", dir))
    }

    /// An app whose `UiMessage` receiver is dropped: the wizard's observer
    /// `send` fails harmlessly, so every state transition still runs.
    pub(crate) fn test_app() -> App {
        App::with_credentials(
            &tokio::sync::mpsc::unbounded_channel().0,
            test_credentials(),
        )
    }

    /// A session whose client never connects (no IO until `connect()`), for
    /// exercising the authenticated shell in-process.
    pub(crate) fn test_session() -> crate::session::Session {
        crate::session::Session {
            handle: "test-handle".to_string(),
            actor_id: test_actor_id(),
            client: fauna_client::NestClient::new(
                "http://127.0.0.1:1".to_string(),
                fauna_core::identity::ActorKeypair::generate(),
            ),
        }
    }

    /// The authenticated fixture's identity secret. One fixed secret, so
    /// [`test_actor_id`] is stable for every test in this binary.
    pub(crate) const TEST_SECRET: [u8; 32] = [7u8; 32];

    /// The actor id [`authed_app`]'s session carries — **derived** from
    /// [`TEST_SECRET`], never a fabricated string.
    ///
    /// It has to be derivable. `AccountRegistry` only ever holds ids it
    /// computed from a secret, and shared-Rust writers guard on membership:
    /// `AccountRegistry::set_supervision_snapshot_json` no-ops for an actor
    /// the index does not know. So a session over an id no registry could
    /// ever contain is a state production cannot reach, and a fixture that
    /// fabricates one silently turns every such guard into a no-op instead
    /// of a test — which is exactly how four `family::tests` went red only
    /// once the guard landed.
    pub(crate) fn test_actor_id() -> String {
        fauna_core::identity::ActorKeypair::from_secret(TEST_SECRET).actor_id_hex()
    }

    pub(crate) fn authed_app() -> App {
        let mut app = test_app();
        // Login always registers the account (`session.rs` calls `add_account`
        // on both the claim and the import path), so an authenticated app
        // whose own registry does not know its actor is not reachable in
        // production. Seed it here so every fixture starts from that state.
        let actor_id = crate::session::registry(&app)
            .add_account(&hex::encode(TEST_SECRET), None, None)
            .expect("seeding the authenticated fixture's account");
        debug_assert_eq!(actor_id, test_actor_id());
        app.session = Some(test_session());
        app
    }

    /// The authenticated fixture must be a state production can actually
    /// reach: its own registry knows its session actor.
    ///
    /// Without this the fixture is strictly weaker than production, and every
    /// shared-Rust writer that guards on registry membership silently degrades
    /// to a no-op under test. That is not hypothetical — it is how
    /// `AccountRegistry::set_supervision_snapshot_json`'s guard (2026-08-28)
    /// reached the public CI as four unexplained `expect` panics in
    /// `family::tests` instead of a red right here.
    #[test]
    fn the_authenticated_fixture_registers_its_own_actor() {
        let app = authed_app();
        let actor = app.session.as_ref().expect("authed").actor_id.clone();
        assert!(
            crate::session::registry(&app)
                .list()
                .iter()
                .any(|a| a.actor_id == actor),
            "authed_app's registry must know its session actor — a fabricated \
             actor id turns every membership guard into a silent no-op"
        );
    }

    // ── W4 phase 4: the offline gate ─────────────────────────────────────
    //
    // `account-data-plane.md` § The offline-mutation contract, class 3. The
    // walk's I6 asserts the general invariant over every reachable state;
    // these pin the specific behaviour in both directions, including the two
    // an offline-only fixture cannot reach (connected, and a page's own
    // stronger reason).

    /// The offline gate, applied to one element exactly as
    /// [`App::page_elements`] applies it.
    fn gated(app: &App, element: crate::element::Element) -> crate::element::Element {
        let mut element = element;
        app.apply_offline_gate(&mut element);
        element
    }

    /// A real `OnlineOnly` affordance — `profile-follow-button` issues
    /// `fauna.subscriptions.subscribe`, which a nest must arbitrate.
    fn follow_button() -> crate::element::Element {
        crate::element::Element::gesture_button(
            ids::PROFILE_FOLLOW_BUTTON,
            "Follow",
            true,
            Gesture::Profile(crate::profile::Action::Follow),
        )
    }

    /// A real offline-capable affordance — `post-submit-button` issues
    /// `fauna.posts.create`, whose id is `blake3(body)`.
    fn submit_post_button() -> crate::element::Element {
        crate::element::Element::gesture_button(
            ids::POST_SUBMIT_BUTTON,
            "Post",
            true,
            Gesture::Feed(crate::feed::Action::SubmitPost),
        )
    }

    #[test]
    fn an_online_only_affordance_desensitizes_offline_and_says_why() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        let gated = gated(&app, follow_button());
        assert!(
            !gated.enabled,
            "an OnlineOnly affordance must not be actuable with no nest"
        );
        assert_eq!(
            gated.label.as_deref(),
            Some(fauna_i18n::strings::common::NEEDS_NEST),
            "and it must say why, per affordance — never a global banner"
        );
    }

    /// Every non-connected transport state means "no nest", not just the
    /// fully-disconnected one.
    #[test]
    fn every_offline_transport_state_desensitizes() {
        for state in [
            ConnectionState::Connecting,
            ConnectionState::Disconnected,
            ConnectionState::Unreachable,
        ] {
            let mut app = authed_app();
            app.connection = state;
            assert!(
                !gated(&app, follow_button()).enabled,
                "{state:?} is not a live nest"
            );
        }
    }

    /// The other direction, and the one that makes the gate worth having: the
    /// classes that work offline keep working. A gate that greyed the whole
    /// screen would pass a one-sided test.
    #[test]
    fn an_offline_capable_affordance_stays_live_offline() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        let gated = gated(&app, submit_post_button());
        assert!(
            gated.enabled,
            "`fauna.posts.create` is OfflineSafe — composing offline is the point"
        );
        assert_eq!(gated.label, None, "and it needs no excuse");
    }

    #[test]
    fn a_live_connection_gates_nothing() {
        let mut app = authed_app();
        app.connection = ConnectionState::Connected;
        assert!(gated(&app, follow_button()).enabled);
        assert!(gated(&app, submit_post_button()).enabled);
    }

    /// A control the page ALREADY disabled is left entirely alone: it is
    /// unavailable for a stronger, more specific reason than the connection
    /// (`folder-webdav-toggle`'s "set up mail first"), and the gate must not
    /// replace that reason with a weaker one.
    #[test]
    fn a_pages_own_disabled_reason_outranks_the_gate() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        let gated = gated(
            &app,
            follow_button().enabled(false).labelled("set up mail first"),
        );
        assert!(!gated.enabled);
        assert_eq!(gated.label.as_deref(), Some("set up mail first"));
    }

    /// And a control the page disabled for its own reason does not acquire the
    /// connection's reason on top. "Needs a connection to your nest" would be a
    /// false explanation for a button that would be dead online too — the
    /// charter's per-feature reason has to be the *true* one.
    #[test]
    fn an_unlabelled_already_disabled_control_gains_no_false_reason() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        let gated = gated(&app, follow_button().enabled(false));
        assert!(!gated.enabled);
        assert_eq!(gated.label, None);
    }

    /// The same precedence on the path that actually reaches the label branch:
    /// an ENABLED control the gate itself desensitizes, which already carries
    /// the page's own hint. It loses its sensitivity, never its prompt — a
    /// checkbox or select whose `labelled` IS its on-screen text would
    /// otherwise be silently retitled "Needs a connection to your nest".
    #[test]
    fn the_gate_keeps_a_labelled_controls_own_prompt() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        let gated = gated(&app, follow_button().labelled("Follow this author"));
        assert!(!gated.enabled, "the gate still desensitizes it");
        assert_eq!(
            gated.label.as_deref(),
            Some("Follow this author"),
            "but it keeps the page's own prompt"
        );
    }

    /// The gate reads the gesture, not the id — so it reaches every actuable
    /// role, not just buttons. A checkbox behind an `OnlineOnly` kind is as
    /// unactuable as a button behind one.
    #[test]
    fn the_gate_covers_every_actuable_role_not_just_buttons() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        // Empty id, exactly as `bridges::setting_elements` builds a real
        // per-bridge toggle — the id is incidental here (this test's whole
        // point is that the gate reads the gesture, not the id).
        let checkbox = crate::element::Element::checkbox_gesture(
            String::new(),
            "Mirror posts",
            false,
            Gesture::Bridges(crate::bridges::Action::Link {
                bridge_id: "nostr".to_string(),
                mode: "generate".to_string(),
            }),
        );
        assert!(!gated(&app, checkbox).enabled);
    }

    /// An inert element has no gesture to classify, so the gate leaves it
    /// alone — a status line does not become "disabled" because the nest is
    /// away.
    #[test]
    fn an_inert_element_is_untouched() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        let label = crate::element::Element::label(ids::CONNECTION_STATUS, "Disconnected");
        let gated = gated(&app, label);
        assert!(gated.enabled);
        assert_eq!(gated.label, None);
    }

    /// The word the gate reads is the same one the `connection-status`
    /// indicator paints — one mapping, so the two can never disagree about
    /// what "connected" means.
    #[test]
    fn the_gates_state_word_matches_the_indicators() {
        let mut app = authed_app();
        for (state, word) in [
            (ConnectionState::Connected, "connected"),
            (ConnectionState::Connecting, "connecting"),
            (ConnectionState::Disconnected, "disconnected"),
            (ConnectionState::Unreachable, "unreachable"),
        ] {
            app.connection = state;
            assert_eq!(app.connection_state_word(), word);
            assert_eq!(
                crate::ui::connection_status_text(state),
                fauna_core::format::connection_state_label(word)
                    .resolve(fauna_i18n::strings::lookup)
            );
        }
    }

    /// **The succession sweep's outcome SURVIVES the account switch that ends
    /// the ceremony — every other authenticated field must not.**
    ///
    /// This pins the one deliberate exception in
    /// [`App::drop_authenticated_state`], and it is load-bearing in a way that
    /// is easy to "clean up" by mistake. The succession ceremony's closing act
    /// *is* an account switch: `adopt_successor` → `switch_account` →
    /// `drop_authenticated_state`. Every other field there is per-identity state
    /// that must never outlive the identity that fetched it, so a future session
    /// tidying the list would naturally add this one too — and that would delete
    /// the ceremony's result at the exact moment the ceremony completed, leaving
    /// the user no way to learn which of their groups were re-pointed or which
    /// members the sweep could not vouch for.
    ///
    /// A full `reset()` is the opposite case and clears it: that destroys every
    /// identity on the box, so there is nothing left for the report to describe.
    // `drop_authenticated_state` tears down the WS supervisor, which needs a
    // reactor — the teardown path under test is inherently async-adjacent.
    #[tokio::test]
    async fn the_succession_sweep_outlives_the_switch_but_not_a_reset() {
        let mut app = authed_app();
        app.errors.insert(Page::Settings, "stale".to_string());
        app.succession_sweep = Some(crate::settings::SweepStatus::NoEngine);

        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert!(
            app.errors.is_empty(),
            "the ordinary per-identity state goes, as it must"
        );
        assert!(
            app.succession_sweep.is_some(),
            "but the ceremony's own outcome survives the switch that ceremony performs — \
             dropping it here deletes the result at the moment of completion"
        );

        app.succession_sweep = Some(crate::settings::SweepStatus::NoEngine);
        app.reset();
        assert!(
            app.succession_sweep.is_none(),
            "a reset destroys every identity, so the report has nothing left to describe"
        );
    }

    /// **Every teardown counts itself, and the count only ever goes up** —
    /// [`App::session_generation`], the observable convention 14's negative
    /// asserts read to prove a gesture did NOT relaunch the session
    /// (`fauna_e2e_agent::SESSION_GENERATION_KEY`).
    ///
    /// The counter moves in exactly one place, [`App::begin_identity_teardown`];
    /// this pins it through [`App::drop_authenticated_state`], the deepest of its
    /// two callers. A test asserting the increment at each *caller* instead would
    /// pass while leaving open the hole that actually shipped — an arm that tears
    /// the session down by some *other* route. That hole is pinned separately, at
    /// the arms themselves, by
    /// [`both_mid_session_escalations_count_and_clear_the_alert_registry`].
    ///
    /// Red-verify by deleting the increment: the first assertion reads 0.
    #[tokio::test]
    async fn every_teardown_bumps_the_session_generation() {
        let mut app = authed_app();
        assert_eq!(
            app.session_generation, 0,
            "a fresh app has torn nothing down yet"
        );

        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert_eq!(
            app.session_generation, 1,
            "the teardown every switch/sign-out shares must count itself"
        );

        // Monotonic: a second teardown adds to the count rather than restating
        // it. This is what lets the helper assert an unchanged *delta* instead
        // of a fixed value, and what makes a late read conservative — it can
        // only ever reveal more teardowns, never fewer.
        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert_eq!(app.session_generation, 2, "the counter accumulates");
    }

    /// **A `DataMessage::Page` result stamped with a STALE session generation is
    /// dropped, never folded** — the generic identity seam
    /// (`DataMessage::Page`'s own doc comment): a page op holds no cancellation handle, so a result
    /// already in flight when an actor change lands still resolves, and without
    /// this check it would fold into whatever page state the INCOMING actor's
    /// `session::establish` has since installed.
    ///
    /// Red-verify by dropping the `session_generation == self.session_generation`
    /// guard from `handle_message`'s `Page` arm: the first assertion fails
    /// because the stale error lands anyway.
    #[test]
    fn a_stale_page_outcome_is_dropped_not_folded() {
        let mut app = authed_app();
        let stale_generation = app.session_generation;
        // Simulate the actor change that landed while this op's read was in
        // flight — exactly `begin_identity_teardown`'s effect.
        app.session_generation += 1;

        app.handle_message(UiMessage::Data(DataMessage::Page(
            stale_generation,
            PageOutcome::Feed(crate::feed::Outcome::Error("stale".to_string())),
        )));

        assert!(
            !app.errors.contains_key(&Page::Feed),
            "a result stamped with a stale session generation must not fold onto \
             the current session's page state"
        );

        // The control: a result stamped with the CURRENT generation still folds
        // normally — the guard drops only stale results, not all of them.
        let generation = app.session_generation;
        app.handle_message(UiMessage::Data(DataMessage::Page(
            generation,
            PageOutcome::Feed(crate::feed::Outcome::Error("fresh".to_string())),
        )));
        assert_eq!(
            app.errors.get(&Page::Feed),
            Some(&"fresh".to_string()),
            "a result stamped with the CURRENT session generation must still fold"
        );
    }

    /// **A teardown that does not route through `drop_authenticated_state` still
    /// owes the identity-teardown signals** — `critical-alerts.md` § Mechanism →
    /// *Lifetime* ("a client MUST drop every alert wherever it tears authenticated
    /// state down — sign-out, account switch, **nest-untrust**, factory reset")
    /// and convention 14's "every arm that tears down counts itself".
    ///
    /// The two mid-session escalation arms are exactly that shape: they cannot
    /// use the full teardown (it zeroes the identity secret their launch-flow
    /// re-entry needs — see [`App::escalate_to_launch_surface`]), so for months
    /// they hand-wrote its first two lines and silently skipped both signals.
    ///
    /// ⚠ The alert half is the one with a live consequence, and it is why this
    /// asserts `teardown_epoch` and not merely an empty `active()`: the epoch is
    /// the ONLY stop signal `critical_alerts::spawn_session_start_sweep`'s
    /// 6-hourly loop watches, and the `NestIdentityChanged` surface's Trust
    /// action re-`route`s straight back into `session::establish`, which spawns a
    /// fresh one. Un-bumped, every re-trust leaks a loop that keeps polling the
    /// public PLC directory for an identity the user has left. An empty registry
    /// with an unmoved epoch would look correct and leak identically.
    ///
    /// Red-verify by restoring either arm's old `session::sign_out(self)` +
    /// `self.launch_machine = None` pair in place of the shared escalation.
    #[tokio::test]
    async fn both_mid_session_escalations_count_and_clear_the_alert_registry() {
        for (arm, escalation) in [
            ("NestIdentityChanged", DataMessage::NestIdentityChanged),
            ("IdentitySuperseded", DataMessage::IdentitySuperseded),
            ("SignInRefused", DataMessage::SignInRefused),
            ("AccountLocked", DataMessage::AccountLocked),
        ] {
            let mut app = authed_app();
            app.alerts.post(
                "atproto-custody:did:plc:departing",
                vec![fauna_core::localized::LocalizedText::key("boom")],
            );
            let epoch_before = app.alerts.teardown_epoch();
            assert!(
                !app.alerts.active().is_empty(),
                "precondition ({arm}): the departing identity's alert must be \
                 standing, or the clear below proves nothing"
            );

            app.handle_message(UiMessage::Data(escalation));

            assert_eq!(
                app.session_generation, 1,
                "{arm} ends the authenticated session, so it must count \
                 itself — a negative assert that reads the counter would otherwise \
                 report 'no relaunch' across a real one"
            );
            assert!(
                app.alerts.active().is_empty(),
                "{arm} must drop the departing identity's alerts — nothing \
                 re-checks a departed DID, so one left standing accuses the next \
                 account un-dismissably"
            );
            assert!(
                app.alerts.teardown_epoch() > epoch_before,
                "{arm} must bump the teardown epoch: it is the only stop \
                 signal the 6-hourly sweep loop watches, and this arm's own \
                 re-entry spawns a fresh one"
            );
        }
    }

    /// **A `reset()` COUNTS itself rather than zeroing the counter** — the one
    /// asymmetry between this field and the barrier probes beside it.
    ///
    /// Tempting to clear here, since `reset()` is the per-test clear point for
    /// every other automation field. It would be wrong: a reset *is* a
    /// teardown, so clearing would erase the very event the counter reports,
    /// and it would make tui's observable a per-app dialect — linux's counter
    /// is process-wide and web's is `sessionStorage`-backed, neither of which
    /// can be cleared per test without losing the same information. Tests read
    /// a delta across their own gesture, so accumulation costs them nothing.
    ///
    /// Red-verify by adding `self.session_generation = 0;` to `reset()`.
    #[tokio::test]
    async fn a_reset_counts_itself_rather_than_zeroing_the_generation() {
        let mut app = authed_app();
        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert_eq!(app.session_generation, 1);

        app.reset();
        assert_eq!(
            app.session_generation, 2,
            "a reset is a teardown and must count itself — zeroing here would \
             erase the event the counter exists to report"
        );
    }

    /// **The successor's owed kit survives the switch too — the same exception,
    /// for the harder reason.**
    ///
    /// The sweep above survives because it *describes* the departing identity.
    /// This one survives because it can only be *performed* by the arriving one:
    /// the mint is authenticated as the successor, and no successor session
    /// exists until `drop_authenticated_state` has run. So the switch is not
    /// incidental to the obligation — it is the boundary the obligation exists
    /// to cross. Dropping the flag here would leave the account with no recovery
    /// kit and no escrow (the succession transaction deletes the old
    /// `recovery_escrow` row) and no signal that anything is owed.
    #[tokio::test]
    async fn the_owed_successor_kit_outlives_the_switch_but_not_a_reset() {
        let mut app = authed_app();
        app.succession_kit_owed = true;

        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert!(
            app.succession_kit_owed,
            "only the successor's own session can mint it, and that session is on the \
             far side of this teardown — dropping it here silently cancels the \
             ceremony's last step"
        );

        app.reset();
        assert!(
            !app.succession_kit_owed,
            "a reset destroys every identity, so there is no successor left to owe"
        );
    }

    /// **And so does the predecessor id that owed kit must seal.**
    ///
    /// Same exception, third field — and the one that is useless alone. The
    /// flag surviving the switch only says a kit is owed; without the id, the
    /// successor's session cannot tell *which* registry row holds the seed the
    /// blob has to carry, so the kit would mint with an empty predecessor
    /// section and the device-loss race stays open for the whole re-seal window
    /// (`succession-aftermath.md` § Re-key scope).
    #[tokio::test]
    async fn the_succession_predecessor_outlives_the_switch_but_not_a_reset() {
        let mut app = authed_app();
        app.succession_predecessor = Some("ab".repeat(32));

        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert_eq!(
            app.succession_predecessor.as_deref(),
            Some("ab".repeat(32).as_str()),
            "the owed kit is minted on the far side of this teardown, and this is what \
             it must seal — dropping it mints a kit with no predecessor section"
        );

        app.reset();
        assert!(
            app.succession_predecessor.is_none(),
            "a reset wipes the registry, so there is no predecessor seed left to seal"
        );
    }

    /// **No page may push an `error-message` element of its own.**
    ///
    /// The id belongs to exactly one producer — `crate::ui::register_frame`,
    /// which registers it from [`App::error_line_text`] so the paint, the
    /// registry and the state protocol's `messages.error` cannot disagree. A
    /// page that pushes its own copy looks right to a human and to
    /// `is_visible`, but is **unreadable by the cross-app `error_text()`**,
    /// which prefers `messages.error` and can never fall through to the element
    /// because tui always emits that key. Five pages did this (admin
    /// web/bridges/dns/logs, settings web); their errors are now read through
    /// [`App::page_snapshot_error`] instead.
    ///
    /// A page needing a *page-scoped* error surface gets its own id (the
    /// `admin-aliases-action-error` discipline), never this one.
    /// Every sub-page is visited, not just the default one: four of the five
    /// offenders were admin sub-pages, which a page-level sweep never reaches.
    #[test]
    fn no_page_pushes_an_error_message_element_of_its_own() {
        let check = |app: &App, what: &str| {
            assert!(
                !app.screen_elements()
                    .iter()
                    .any(|e| e.id == crate::ui::ERROR_MESSAGE_ID),
                "{what} pushes its own `error-message` — route it through \
                 `App::page_snapshot_error` (or give it a page-scoped id, the \
                 `admin-aliases-action-error` discipline) so `error_text()` can \
                 read it"
            );
        };
        for page in crate::pages::Page::ALL {
            let mut app = authed_app();
            app.page = page;
            check(&app, &format!("{page:?}"));
        }
        for sub in crate::admin::AdminPage::BUILT {
            let mut app = authed_app();
            app.page = crate::pages::Page::Admin;
            app.admin.sub = *sub;
            check(&app, &format!("Admin/{sub:?}"));
        }
        let mut kinds: Vec<u8> = Vec::new();
        for sub in crate::settings::ALL_SUBPAGES {
            let mut app = authed_app();
            app.page = crate::pages::Page::Settings;
            app.settings.sub = *sub;
            check(&app, &format!("Settings/{sub:?}"));
            // Also drives the compiler's half of `ALL_SUBPAGES`: `subpage_kind`
            // is an exhaustive match, so a new variant breaks the build here
            // until it is listed, and its `debug_assert` catches the const
            // being the half that was forgotten.
            kinds.push(crate::settings::subpage_kind(*sub));
        }
        let unique: std::collections::BTreeSet<u8> = kinds.iter().copied().collect();
        assert_eq!(
            unique.len(),
            crate::settings::ALL_SUBPAGES.len(),
            "ALL_SUBPAGES lists a sub-page twice"
        );
    }

    /// `security.md` § Post-auth surfacing: a mid-session identity verdict hands
    /// the screen to the BLOCKING launch surface and **keeps the credentials**.
    ///
    /// Both halves are pinned because both have a wrong version that compiles and
    /// looks right: routing the verdict to the error/toast path (a soft surface
    /// over a session that can no longer graduate — a generic-error mystery
    /// instead of the honest verdict), and tearing down with `App::reset` instead
    /// of `session::sign_out` (which wipes the credential namespace — but nothing
    /// is wrong with the *identity*, and BOTH exits from the surface, re-trust and
    /// "use a different nest", need it in hand).
    ///
    /// The account is SEEDED first on purpose: against the empty store
    /// `test_app()` builds, `stored_account(..).is_some()` is false before and
    /// after, so the credentials-survive assertion would pass identically whether
    /// the handler kept them or wiped them — the pass-value-cannot-discriminate
    /// class this app has now hit seven times. Asserting behind the precondition
    /// is what makes it a measurement.
    #[tokio::test]
    async fn a_post_auth_identity_verdict_blocks_the_session_but_keeps_the_credentials() {
        let mut app = authed_app();
        let secret_hex = hex::encode([7u8; 32]);
        let registry = crate::session::registry(&app);
        let actor_id = registry
            .add_account(&secret_hex, Some("http://127.0.0.1:1"), None)
            .expect("seeding the active account");
        let _ = registry.set_active(&actor_id);
        assert!(
            crate::session::stored_account(&app).is_some(),
            "precondition: the seeded account must be readable, or the \
             credentials-survive assertion below proves nothing"
        );
        assert!(
            !app.showing_launch_surface(),
            "precondition: the authenticated pages own the screen before the verdict"
        );

        app.handle_message(UiMessage::Data(DataMessage::NestIdentityChanged));

        assert!(
            app.session.is_none() && app.showing_launch_surface(),
            "a possible-MITM verdict must BLOCK the session and hand the screen to \
             the launch surface — not a banner, badge or toast over a session whose \
             connections can no longer graduate"
        );
        assert!(
            crate::session::stored_account(&app).is_some(),
            "credentials must SURVIVE the teardown: nothing is wrong with the \
             identity — the nest changed — and both exits from the surface \
             (re-trust, use a different nest) need the identity in hand"
        );
    }

    /// Once the registration chain has PROVEN the successor, the superseded
    /// screen may name it — the upgrade from the claim-free message to
    /// `identity_superseded_verified` (`identity-succession.md` § Propagation →
    /// *Own device fleet*). The claim-free message is what ships until then, so
    /// this transition is the entire difference between "something happened" and
    /// "your account is now X".
    #[test]
    fn a_verified_successor_upgrades_the_superseded_message_and_names_it() {
        let mut app = test_app();
        let successor = "5c".repeat(32);

        // The state the launch route leaves behind (`launch::route_superseded_to_import`),
        // set through the same shared transition it calls — reproduced here rather
        // than invoked, because that fn also spawns the verify this test supplies
        // the answer for.
        app.wizard.machine.begin_import_identity_with_reason(
            fauna_i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED.to_string(),
        );
        let before = app
            .wizard
            .error_text()
            .expect("the claim-free reason shows");
        assert!(
            !before.contains(&successor),
            "precondition: nothing is named before the chain verifies it"
        );

        app.handle_message(UiMessage::Data(DataMessage::IdentitySupersededVerified {
            successor: successor.clone(),
            predecessor: "4b".repeat(32),
        }));

        let after = app.wizard.error_text().expect("the reason still shows");
        assert!(
            after.contains(&successor),
            "a VERIFIED successor is a fact the user needs in order to confirm they \
             are importing the right seed; got {after:?}"
        );
        assert_eq!(
            app.wizard.machine.step(),
            fauna_onboarding_machine::OnboardingStep::IdentityImport,
            "naming the successor must not move the user off the import screen"
        );
    }

    /// The **join** the corpus-re-seal line depends on: the reading the sync
    /// agent's status poll folds has to actually land on `App::aftermath`, where
    /// the Settings render reads it.
    ///
    /// ⚠ Not a formality. Exhaustiveness stops the arm being *deleted*, but not
    /// an arm that compiles and drops the value — and every Settings render test
    /// would stay green through that, because they all set the field by hand.
    /// That is the mark→row finding one leg over: a surface whose data comes
    /// from another plane owes a test at the join, not only at the render.
    /// Verified by mutation — an arm rewritten to discard the reading reds this
    /// test and nothing else.
    ///
    /// The second message asserts the level semantics the type doc promises:
    /// unlike the four in-process legs, this one is re-sent on every poll tick,
    /// so a later reading must REPLACE the earlier one rather than be ignored as
    /// a duplicate or accumulated alongside it.
    #[test]
    fn a_polled_corpus_reseal_reading_lands_where_settings_reads_it() {
        use fauna_client_sync::agent::{CorpusResealOutcome, CorpusResealProgress};
        let mut app = tests::authed_app();
        assert!(
            app.aftermath.corpus_reseal.is_none(),
            "precondition: nothing has been polled yet"
        );

        app.handle_message(UiMessage::Data(DataMessage::CorpusResealProgress(
            CorpusResealProgress::Running,
        )));
        assert_eq!(
            app.aftermath.corpus_reseal,
            Some(CorpusResealProgress::Running),
            "the agent's reading must reach the state the Settings render reads"
        );

        app.handle_message(UiMessage::Data(DataMessage::CorpusResealProgress(
            CorpusResealProgress::Settled(CorpusResealOutcome::Resealed {
                resealed: 40,
                owed: 0,
            }),
        )));
        assert_eq!(
            app.aftermath.corpus_reseal,
            Some(CorpusResealProgress::Settled(
                CorpusResealOutcome::Resealed {
                    resealed: 40,
                    owed: 0
                }
            )),
            "this leg is a LEVEL re-sent every poll tick — the newest reading wins"
        );
    }

    /// The verify is a best-effort round trip that can land long after the user
    /// moved on. Re-asserting a supersession over whatever they are doing now
    /// would be a banner from a flow they already dealt with — so the upgrade is
    /// guarded on still being on the screen it upgrades.
    #[test]
    fn a_late_verification_does_not_reassert_itself_after_the_user_moved_on() {
        let mut app = test_app();
        app.wizard.machine.begin_create_identity();
        let step_before = app.wizard.machine.step();

        app.handle_message(UiMessage::Data(DataMessage::IdentitySupersededVerified {
            successor: "5c".repeat(32),
            predecessor: "4b".repeat(32),
        }));

        assert_eq!(
            app.wizard.machine.step(),
            step_before,
            "a late verification must not drag the user back to the import screen"
        );
    }

    /// Any teardown ends the ceremony's claim on the session: its result is
    /// stale from then on, so a flag left set would defer every later
    /// supersession on behalf of a fold that will never run.
    #[tokio::test]
    async fn a_teardown_clears_the_in_flight_ceremony() {
        let mut app = authed_app();
        app.stolen_ceremony_in_flight = true;
        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert!(!app.stolen_ceremony_in_flight);
    }

    /// **A relaunch refused as superseded, whose VERIFIED successor this device
    /// already holds, signs in as it** — the way back in the undecidable
    /// arm's message promises (*"reopen the app to sign in as it"*). The
    /// lost-reply ceremony persisted the successor before anything could fail,
    /// so the seed is in the store while the old identity is still active;
    /// sending the user to import a key they were never shown would strand
    /// them.
    ///
    /// Also pins what the ceremony's fold would have done and a relaunch must
    /// still do: record the succession link (the corpus re-seal reads it) and
    /// owe the successor its recovery kit (the old one retired with the old
    /// identity).
    ///
    /// Red-verify by making `adopt_held_successor` return `false`: the active
    /// account stays the predecessor.
    #[tokio::test]
    async fn a_verified_successor_this_device_holds_is_adopted_not_imported() {
        let mut app = test_app();
        let registry = crate::session::registry(&app);
        let predecessor = registry
            .add_account(&hex::encode([7u8; 32]), Some("http://127.0.0.1:1"), None)
            .expect("seeding the refused identity");
        registry.set_active(&predecessor).expect("activating it");
        let successor = registry
            .add_account(&hex::encode([9u8; 32]), Some("http://127.0.0.1:1"), None)
            .expect("seeding the successor the ceremony persisted");
        assert_eq!(
            registry.active().as_deref(),
            Some(predecessor.as_str()),
            "precondition: the lost-reply state — successor held, predecessor active"
        );
        app.wizard.machine.begin_import_identity_with_reason(
            fauna_i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED.to_string(),
        );

        app.handle_message(UiMessage::Data(DataMessage::IdentitySupersededVerified {
            successor: successor.clone(),
            predecessor: predecessor.clone(),
        }));

        let registry = crate::session::registry(&app);
        assert_eq!(
            registry.active().as_deref(),
            Some(successor.as_str()),
            "a device holding the proven successor's key must sign in as it"
        );
        assert_eq!(
            registry.terminal_successor_of(&predecessor).as_deref(),
            Some(successor.as_str()),
            "the succession link must be recorded — the corpus re-seal needs it"
        );
        assert!(
            app.succession_kit_owed,
            "the successor never got its kit from the ceremony; it is still owed one"
        );
    }

    /// The control: a verified successor this device does NOT hold is the
    /// ordinary case — another device ran the ceremony — and only upgrades the
    /// message. Nothing is adopted and the user stays on the import screen.
    #[tokio::test]
    async fn a_verified_successor_this_device_does_not_hold_only_names_it() {
        let mut app = test_app();
        app.wizard.machine.begin_import_identity_with_reason(
            fauna_i18n::strings::onboarding::launch::IDENTITY_SUPERSEDED.to_string(),
        );
        let successor = "5c".repeat(32);

        app.handle_message(UiMessage::Data(DataMessage::IdentitySupersededVerified {
            successor: successor.clone(),
            predecessor: "4b".repeat(32),
        }));

        assert_eq!(
            app.wizard.machine.step(),
            fauna_onboarding_machine::OnboardingStep::IdentityImport
        );
        assert!(!app.succession_kit_owed);
        assert!(
            app.wizard
                .error_text()
                .is_some_and(|t| t.contains(&successor)),
            "the verified successor is still named for the user to import"
        );
    }

    /// Convention 11's refusal slot must report on EVERY screen — including the
    /// launch/wizard surface, which is where a signed-out app lives.
    ///
    /// This is not hypothetical tidiness: the honest refusal reason for a
    /// session-scoped agent command (`silent_sign_in` → "no active account") is
    /// *precisely* the pre-auth state. While the check sat inside the
    /// `!showing_launch_surface()` branch, such a refusal was recorded and never
    /// painted — the driver read `error=''` and the failure looked like a product
    /// bug three assertions later. Same shape as the `injected_error` slot that
    /// nav silently cleared.
    #[test]
    fn a_refused_command_reports_on_the_launch_surface_too_not_just_on_a_page() {
        let mut signed_out = test_app();
        assert!(
            signed_out.showing_launch_surface(),
            "precondition: a signed-out app is showing the launch surface — the \
             screen this test exists to cover"
        );
        signed_out.report_refused_agent_command("silent_sign_in (no active account)");
        let reported = signed_out.screen_error_text().unwrap_or_default();
        assert!(
            reported.contains("silent_sign_in"),
            "a refusal must surface while the launch surface owns the screen; got {reported:?}"
        );

        // And it still outranks a page's own error once signed in.
        let mut authed = authed_app();
        authed
            .errors
            .insert(authed.page, "a page-level error".to_string());
        authed.report_refused_agent_command("some_command");
        assert!(
            authed
                .screen_error_text()
                .unwrap_or_default()
                .contains("some_command"),
            "a refusal outranks the page's own error — the app never did what the \
             driver asked, so the page error describes a state the test never set up"
        );
    }

    /// The append-mode surface predicate: a launch/wizard surface owns the screen
    /// when signed out, AND when `adding_account` is set over a live session — the
    /// whole point of append mode. This is what routes the five screen projections
    /// to the wizard while the session stays live underneath.
    #[test]
    fn showing_launch_surface_covers_signed_out_and_append_over_a_live_session() {
        assert!(
            test_app().showing_launch_surface(),
            "signed out → a launch surface owns the screen"
        );
        let authed = authed_app();
        assert!(
            !authed.showing_launch_surface(),
            "authenticated with no append in flight → the authenticated pages own the screen"
        );
        let mut adding = authed_app();
        adding.adding_account = true;
        assert!(
            adding.showing_launch_surface(),
            "append over a live session → the wizard owns the screen though the session is live"
        );
    }

    /// Entering "Add account" hands the screen to a FRESH wizard (create-or-import
    /// from scratch — a reset machine starts at `IdentityChoice`) over the live
    /// session, which is NOT torn down.
    #[test]
    fn enter_add_account_shows_a_fresh_wizard_over_the_live_session() {
        use fauna_onboarding_machine::OnboardingStep;
        let mut app = authed_app();
        app.enter_add_account();
        assert!(app.adding_account, "append mode is armed");
        assert!(app.authenticated(), "the live session is NOT torn down");
        assert!(
            app.showing_launch_surface(),
            "the wizard surface owns the screen"
        );
        assert_eq!(
            app.wizard.machine.step(),
            OnboardingStep::IdentityChoice,
            "a fresh append wizard starts at create-or-import, not a seeded step"
        );
        // The screen presents as the wizard: no authenticated sidebar tabs.
        let ids: Vec<String> = app.screen_elements().into_iter().map(|e| e.id).collect();
        assert!(
            !ids.iter().any(|id| id.ends_with("-tab")),
            "append mode must not paint the authenticated sidebar tabs; got {ids:?}"
        );
    }

    /// Abandoning append mode returns to the live session (a pure return — the
    /// session was never dropped).
    #[test]
    fn abandon_add_account_returns_to_the_live_session() {
        let mut app = authed_app();
        app.enter_add_account();
        app.abandon_add_account();
        assert!(!app.adding_account, "append mode is cleared");
        assert!(app.authenticated(), "the live session survives the abandon");
        assert!(
            !app.showing_launch_surface(),
            "the authenticated pages own the screen again after abandon"
        );
    }

    /// Escape abandons append mode — and it must WIN over the lower Esc arms. The
    /// session's settings sub-page is left on `Account` beneath the wizard (that is
    /// where "Add account" is tapped), so without the highest-priority append arm
    /// this Esc would fall through to the settings-back arm and reset the sub-page
    /// instead of leaving the wizard.
    #[test]
    fn esc_abandons_append_mode_over_the_settings_back_arm() {
        let mut app = authed_app();
        app.settings.sub = crate::settings::SubPage::Account;
        app.enter_add_account();
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.adding_account, "Esc abandoned append mode");
        assert_eq!(
            app.settings.sub,
            crate::settings::SubPage::Account,
            "the append Esc arm must win: the underlying settings sub-page is untouched"
        );
    }

    /// Every page's field must round-trip through the **dispatcher**, into that
    /// page's own state.
    ///
    /// The nesting (`Field::Events(EventsField::…)`) makes cross-page
    /// mis-routing unrepresentable — the type system, not this test, is what
    /// retires the twice-shipped "a typed value silently landed in wizard
    /// state" bug. What the compiler still can't see is a page wiring its own
    /// variant to the wrong buffer (`set_field` writing A while `field` reads
    /// B), so this drives every page through `App::set_field`/`App::field` —
    /// the same door `/element/type` and the keyboard use — rather than
    /// poking page state directly, which is precisely how the invite-field bug
    /// slipped past its unit test.
    #[test]
    fn every_page_s_field_round_trips_through_the_dispatcher() {
        use crate::contacts::ContactsField;
        use crate::conversations::ConversationsField;
        use crate::element::Field;
        use crate::events::EventsField;
        use crate::feed::FeedField;
        use crate::profile::ProfileField;
        use crate::search::SearchField;
        use crate::settings::SettingsField;
        use crate::unlock::UnlockField;
        use crate::wizard::WizardField;

        let mut app = authed_app();
        // The profile edit buffers exist only while the form is open.
        let _ = crate::profile::apply_local(&mut app, crate::profile::Action::OpenEdit);

        // One local-buffer field per page. Manager-backed fields (the feed
        // composer, the conversations drafts) read off a snapshot that a bare
        // test app has no manager for — their own page suites cover those.
        let cases: Vec<(Field, &str)> = vec![
            (Field::Wizard(WizardField::Handle), "ada@example.com"),
            (Field::Feed(FeedField::CreateFeedName), "my-feed"),
            (
                Field::Conversations(ConversationsField::ThreadRename),
                "renamed-thread",
            ),
            (Field::Contacts(ContactsField::Filter), "ada"),
            (Field::Unlock(UnlockField::Passphrase), "correct horse"),
            (Field::Profile(ProfileField::DisplayName), "Ada Lovelace"),
            (Field::Events(EventsField::Summary), "standup"),
            (Field::Settings(SettingsField::NewHandle), "new-handle"),
            (Field::Search(SearchField::Query), "search terms"),
        ];

        for (field, value) in cases {
            let _ = app.set_field(field.clone(), value.to_string());
            assert_eq!(
                app.field(field.clone()),
                value,
                "{field:?} did not round-trip through the dispatcher — its page \
                 wrote one buffer and read another"
            );
        }

        // The fall-through owner is gone: a non-wizard write cannot reach
        // wizard state (`inputs` is keyed by `WizardField`, so it is a type
        // error to try), and the wizard's own write is the only thing in there.
        assert_eq!(app.wizard.field(WizardField::Handle), "ada@example.com");
    }

    #[test]
    fn navigation_moves_through_all_pages_and_clamps() {
        let mut app = authed_app();
        assert_eq!(app.page, Page::Feed);
        app.handle_key(key(KeyCode::Up)); // clamps at the top
        assert_eq!(app.page, Page::Feed);
        for expected in &Page::ALL[1..] {
            app.handle_key(key(KeyCode::Down));
            assert_eq!(app.page, *expected);
        }
        // Page::Exit is the true last row (below Settings) — not admin/family
        // gated, so it always trails Page::ALL directly here.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.page, Page::Exit);
        app.handle_key(key(KeyCode::Down)); // clamps at the bottom
        assert_eq!(app.page, Page::Exit);
    }

    /// The bottom-of-sidebar `Exit Fauna` row: the ring can rest on it like any
    /// other row (an empty preview pane, no quit yet), and ONLY actuating it —
    /// Enter or a click — sets `should_quit`. Covers both actuation paths,
    /// since `click_sidebar` funnels through the same `actuate_focused` door.
    #[test]
    fn exit_row_quits_only_on_actuation_not_on_ring_arrival() {
        let mut app = authed_app();
        let last = app.sidebar_pages().len() - 1;
        for _ in 0..last {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(app.page, Page::Exit, "the ring reached the last row");
        assert!(!app.should_quit, "arriving on the row must not itself quit");

        app.handle_key(key(KeyCode::Enter));
        assert!(app.should_quit, "actuating the row quits");
    }

    #[test]
    fn clicking_the_exit_row_quits() {
        let mut app = authed_app();
        let last = app.sidebar_pages().len() - 1;
        app.click_sidebar(last);
        assert_eq!(app.page, Page::Exit);
        assert!(
            app.should_quit,
            "a click actuates immediately, like any other row"
        );
    }

    #[test]
    fn unauthenticated_arrows_move_the_wizard_focus_ring_not_the_sidebar() {
        let mut app = test_app();
        assert!(!app.authenticated());
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.page, Page::Feed, "nav hidden_when unauthenticated");
        assert_eq!(
            app.focused().map(|e| e.id).as_deref(),
            Some("import-identity-button"),
            "Down moves the wizard focus ring"
        );
    }

    /// The keymap is now **one** map for every screen, so `q` is governed by a
    /// single rule rather than by which screen you happen to be on: a printable
    /// key types into a focused `Input`, and only otherwise acts as a command.
    ///
    /// That is what makes the shell able to host a page with a composer at all
    /// (the old shell bound bare `q` to quit unconditionally, so a composer
    /// could never receive the letter) while keeping the property the wizard
    /// needed: `q` cannot quit out from under someone typing a handle.
    #[test]
    fn q_quits_unless_an_input_has_focus() {
        use fauna_onboarding_machine::OnboardingStep;

        // Shell, focus on the sidebar (a tab, not an input) → `q` quits.
        let mut app = authed_app();
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);

        // Ctrl+C quits regardless — including out of a focused input.
        let mut app = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);

        // A focused input swallows the letter: it is text, not a command.
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        assert!(matches!(
            app.focused().map(|e| e.role),
            Some(crate::element::Role::Input(_))
        ));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit, "`q` is text while an input has focus");
        assert_eq!(app.wizard.field(WizardField::Handle), "q");
    }

    /// `post_detail` (and `create_feed`) get no ui.yaml dismiss element — the
    /// agent never opens one it doesn't also close via a fresh `reset` — so this
    /// is a keymap-only escape, pinned here per the "keyboard is human-only
    /// surface" rule.
    #[test]
    fn esc_leaves_a_feed_sub_page_back_to_the_list() {
        let mut app = authed_app();
        app.feed.mode = crate::feed::Mode::PostDetail("post-id".to_string());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.feed.mode, crate::feed::Mode::List);

        // A no-op once already on the list.
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.feed.mode, crate::feed::Mode::List);
    }

    /// The shell's two focus zones. A single flat ring over "sidebar + page"
    /// would make switching pages a Tab-through-every-post-on-the-feed.
    #[test]
    fn arrows_move_within_a_zone_and_left_right_switch_zones() {
        let mut app = authed_app();
        // Lands in the sidebar: at login the page pane may hold nothing
        // focusable, and a user with no visible focus can't discover ←/→.
        assert_eq!(app.zone, Zone::Sidebar);
        assert_eq!(app.focused().map(|e| e.id).as_deref(), Some("feed-tab"));

        // → hands the keyboard to the page pane; ← takes it back.
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.zone, Zone::Page);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.zone, Zone::Sidebar);

        // ↓ in the sidebar is navigation (selection *is* the ring), so the
        // painted page follows the highlight with no separate confirm step.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.page, Page::Conversations);
        assert_eq!(
            app.focused().map(|e| e.id).as_deref(),
            Some("conversations-tab")
        );

        // Enter on a tab commits and steps into the page.
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.zone, Zone::Page);
        assert_eq!(app.page, Page::Conversations);
    }

    /// The one-list invariant, now extended to the authenticated shell: the
    /// sidebar is `Element`s like everything else, so paint, the registry and
    /// the focus ring cannot disagree about what the shell shows.
    #[test]
    fn the_shell_registers_its_sidebar_through_the_one_element_list() {
        let app = authed_app();
        let ids: Vec<String> = app.screen_elements().into_iter().map(|e| e.id).collect();
        for page in Page::ALL {
            assert!(
                ids.contains(&page.tab_id().to_string()),
                "{} must come from screen_elements(), not a hand-rolled \
                 registration path that can drift from paint",
                page.tab_id()
            );
        }
        // …and the current page's own elements follow the tabs.
        assert!(ids.contains(&"feed-view".to_string()));
    }

    #[test]
    fn typing_into_a_focused_input_writes_the_field() {
        let mut app = test_app();
        // Land on handle_entry, where the first focusable is `handle-input`.
        app.wizard
            .machine
            .set_step_for_test(fauna_onboarding_machine::OnboardingStep::HandleEntry);
        assert!(matches!(
            app.focused().map(|e| e.role),
            Some(crate::element::Role::Input(Field::Wizard(
                WizardField::Handle
            )))
        ));
        for c in "ab".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.wizard.field(WizardField::Handle), "ab");
        // The handle lives in the machine, not a client-side buffer.
        assert_eq!(app.wizard.machine.current_handle(), "ab");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.wizard.field(WizardField::Handle), "a");
    }

    #[test]
    fn enter_on_a_disabled_button_is_inert() {
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(fauna_onboarding_machine::OnboardingStep::HandleEntry);
        // Focus the Check button; it is disabled while the handle is empty.
        app.focus_next();
        let focused = app.focused().expect("check button");
        assert_eq!(focused.id, "handle-check-button");
        assert!(
            !focused.enabled,
            "Check is disabled while the input is empty"
        );
        app.handle_key(key(KeyCode::Enter));
        assert!(app.wizard.error.is_none());
    }

    /// The Mail → Spam threshold draft, addressed through the same generic
    /// `Field` door the agent's `type` route and the human's `Char` arm both
    /// use — never the page struct's private buffer, so these pins bind the
    /// contract rather than a layout.
    fn threshold_field() -> Field {
        Field::Settings(crate::settings::SettingsField::MailSpamThresholdOverride)
    }

    /// The `KeyCode::Char` twin of [`enter_on_a_disabled_button_is_inert`], and
    /// the human half of convention 11's actuation clause.
    ///
    /// **The gap this closes.** [`App::actuate_focused`]'s own comment already
    /// declares the rule — *"A disabled button is inert here exactly as it is
    /// for the agent's click — one gate, both paths"* — but the `Char` arm
    /// beside it reached [`App::focused_input`], which matched on `role` alone
    /// and never read `enabled`. So the ring could sit on an input the offline
    /// gate had DIMMED and a keystroke still wrote the field, while `Enter` on
    /// that same element did nothing: the user types a whole value into a
    /// greyed-out box and the commit silently refuses.
    ///
    /// **The subject is a real reachable instance, not a synthetic one.**
    /// `mail-spam-threshold-override-input` is an `input_commit` whose gesture
    /// is `fauna.bridges.set_spam_threshold_override`, classified `OnlineOnly`
    /// (`fauna_protocol::offline_class`), so [`App::apply_offline_gate`]
    /// desensitizes it whenever the transport is not connected. A plain
    /// `Role::Input` carries no gesture and is therefore never offline-gated —
    /// an `InputCommit` under the offline gate is the ONLY way a tui input is
    /// disabled today, which is exactly why this hole survived so long.
    #[test]
    fn typing_into_a_disabled_input_is_inert() {
        let mut app = authed_app();
        app.connection = ConnectionState::Disconnected;
        app.page = Page::Settings;
        app.settings.sub = crate::settings::SubPage::MailSpam;

        let elements = app.page_elements();
        let index = elements
            .iter()
            .position(|e| e.id == ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT)
            .expect("the threshold input paints on the Mail → Spam sub-page");
        assert!(
            !elements[index].enabled,
            "premise: an OnlineOnly input_commit is desensitized while disconnected — \
             if this ever reads enabled the whole test is vacuous"
        );
        // Seat the ring on it exactly as `click_page_element` does.
        app.zone = Zone::Page;
        app.focus = elements[..index].iter().filter(|e| e.focusable()).count();
        assert_eq!(
            app.focused().map(|e| e.id.clone()),
            Some(ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT.to_string())
        );

        app.handle_key(key(KeyCode::Char('9')));
        assert_eq!(
            app.field(threshold_field()),
            "",
            "a keystroke on a DISABLED input must not write the field — the ring \
             may rest there (disabled elements stay focusable so a reader can \
             inspect them), but the field is not editable"
        );

        // Backspace rides the same one door, so it is inert for free — pinned
        // rather than assumed, since a future refactor could re-split them.
        let _ = app.set_field(threshold_field(), "7".to_string());
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            app.field(threshold_field()),
            "7",
            "Backspace reaches the field through the same `focused_input` door"
        );
    }

    /// The gate is PRECISE: the same input, connected, types normally.
    ///
    /// The regression that would matter most. `focused_input` is on the hot
    /// path of every keystroke in the app, so a predicate that over-refuses
    /// would not fail one test — it would make the client un-typeable.
    #[test]
    fn typing_into_that_same_input_works_when_it_is_enabled() {
        let mut app = authed_app();
        app.connection = ConnectionState::Connected;
        app.page = Page::Settings;
        app.settings.sub = crate::settings::SubPage::MailSpam;

        let elements = app.page_elements();
        let index = elements
            .iter()
            .position(|e| e.id == ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT)
            .expect("the threshold input paints on the Mail → Spam sub-page");
        assert!(
            elements[index].enabled,
            "premise: connected, nothing desensitizes it"
        );
        app.zone = Zone::Page;
        app.focus = elements[..index].iter().filter(|e| e.focusable()).count();

        app.handle_key(key(KeyCode::Char('9')));
        assert_eq!(app.field(threshold_field()), "9");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.field(threshold_field()), "");
    }

    #[test]
    fn an_unparseable_pasted_secret_errors_without_touching_the_machine() {
        use fauna_onboarding_machine::OnboardingStep;
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::IdentityImport);
        app.wizard
            .set_field(WizardField::ImportSecret, "not-a-secret".to_string());

        let mut dispatched = false;
        app.dispatch_wizard(crate::wizard::Action::ConfirmImportedIdentity, |_, _, _| {
            dispatched = true;
        });
        assert!(!dispatched, "a bad paste never reaches the machine");
        assert!(
            app.wizard.error.is_some(),
            "invalid_secret surfaces client-side"
        );
        assert_eq!(app.wizard.machine.step(), OnboardingStep::IdentityImport);
    }

    #[test]
    fn a_valid_pasted_secret_carries_its_handle_into_the_machine() {
        use fauna_onboarding_machine::OnboardingStep;
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::IdentityImport);
        let secret = "a".repeat(64);
        app.wizard.set_field(
            WizardField::ImportSecret,
            format!("fauna://identity?secret={secret}&handle=alice@example.com"),
        );

        let mut payload = None;
        app.dispatch_wizard(crate::wizard::Action::ConfirmImportedIdentity, |_, _, p| {
            payload = Some(p);
        });
        assert_eq!(payload.as_deref(), Some(secret.as_str()));
        assert!(app.wizard.error.is_none());
        assert_eq!(app.wizard.machine.current_handle(), "alice@example.com");
    }

    #[test]
    fn connection_state_message_updates_status() {
        let mut app = test_app();
        assert_eq!(app.connection, ConnectionState::Disconnected);
        app.handle_message(UiMessage::Data(DataMessage::ConnectionState(
            ConnectionState::Connecting,
        )));
        assert_eq!(app.connection, ConnectionState::Connecting);
    }

    #[test]
    fn reset_returns_the_wizard_to_identity_choice() {
        use fauna_onboarding_machine::OnboardingStep;
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        app.wizard
            .set_field(WizardField::Handle, "alice@example.com".to_string());
        app.reset();
        assert_eq!(app.wizard.machine.step(), OnboardingStep::IdentityChoice);
        assert_eq!(app.wizard.field(WizardField::Handle), "");
    }

    /// The `sign-out-residue` view paints on `identity_choice` while a residue
    /// owes work — and nowhere else: not on a later wizard page, and never over
    /// an append-mode "Add account" wizard, whose user is signed in.
    #[test]
    fn the_residue_view_paints_on_identity_choice_alone() {
        use fauna_onboarding_machine::OnboardingStep;
        const RESIDUE_IDS: [&str; 3] = [
            "sign-out-residue",
            "sign-out-residue-message",
            "sign-out-residue-retry-button",
        ];
        let painted = |app: &App| -> Vec<String> {
            app.page_elements()
                .into_iter()
                .map(|e| e.id)
                .filter(|id| RESIDUE_IDS.contains(&id.as_str()))
                .collect()
        };
        let mut app = test_app();
        app.launch = crate::launch::LaunchSurface::Wizard;
        assert!(painted(&app).is_empty(), "no residue, no view");

        app.sign_out_residue = Some(crate::account_scope::ResidueSurface {
            record: fauna_client_accounts::SignOutResidue::record(
                &[std::path::PathBuf::from("/nowhere/aa11")],
                &fauna_client_accounts::CredentialSweep::default(),
            ),
            line: "residue".to_string(),
        });
        assert_eq!(painted(&app), RESIDUE_IDS, "all three, on identity_choice");
        let retry = app
            .page_elements()
            .into_iter()
            .find(|e| e.id == "sign-out-residue-retry-button")
            .expect("the retry control");
        assert!(
            matches!(
                retry.gesture(),
                Some(crate::element::Gesture::RetrySignOutResidue)
            ),
            "Remove Again drives the residue retry"
        );

        app.adding_account = true;
        assert!(painted(&app).is_empty(), "never over an append-mode wizard");
        app.adding_account = false;

        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        assert!(painted(&app).is_empty(), "identity_choice alone");
    }

    #[test]
    fn reset_keeps_the_recovery_kit_declaration() {
        // `Wizard::new` declares the capability ONCE per process
        // (`set_renders_recovery_kit(true)` — tui is the lead app that has the
        // screen). `App::reset()` is what the e2e `app` fixture calls before
        // every test against one session-scoped app process, so a reset that
        // dropped the declaration would strand every later test on a flow that
        // skips a screen tui does render — green alone, red in a batch. The
        // machine-side rule is pinned by
        // `recovery_kit_navigation::a_factory_reset_preserves_the_apps_declared_capability`;
        // this pins tui's own path to it.
        use fauna_onboarding_machine::OnboardingStep;
        let mut app = test_app();

        app.reset();

        app.wizard.machine.begin_create_identity();
        app.wizard
            .machine
            .confirm_generated_identity()
            .expect("generated secret exists after begin_create_identity");
        assert_eq!(
            app.wizard.machine.step(),
            OnboardingStep::RecoveryKit,
            "a post-reset re-onboard still routes through tui's recovery-kit screen"
        );
    }

    #[test]
    fn reset_clears_the_whole_credential_namespace() {
        // A factory reset must leave nothing for the next launch to hydrate —
        // the per-actor slots AND the registry's `fauna/index` active-account
        // pointer. Keeping the index alone re-elects the reset actor, and the
        // next sign-in then 403-hangs on the bearer/keypair mismatch
        // (`fauna_credential_store::cred_file_remove`).
        use fauna_client_accounts::{AccountRegistry, SecretStore};
        let mut app = test_app();
        let store = Arc::clone(&app.credentials);

        // A signed-in actor: account #1, active.
        let registry = AccountRegistry::new(Arc::clone(&store) as Arc<dyn SecretStore>);
        let actor = registry
            .add_account(&"a".repeat(64), Some("https://a.example"), None)
            .expect("the actor registers");
        assert!(registry.active().is_some(), "the actor is active pre-reset");

        app.reset();

        assert_eq!(store.get("fauna/index"), None, "the account index is gone");
        assert_eq!(store.get(&format!("fauna/{actor}/secret")), None);
        assert_eq!(
            AccountRegistry::new(Arc::clone(&store) as Arc<dyn SecretStore>).active(),
            None,
            "no active account survives a factory reset"
        );
    }

    /// A sign-out's stop does not hold tui's UI loop, and nothing queued
    /// behind it — the erase, a second sign-out's erase — runs until it ends.
    /// The tui twin of linux's
    /// `the_stop_hands_the_thread_back_and_the_erase_waits_for_it`.
    ///
    /// Latency-independent by construction: the stop waits on an assembly
    /// still in flight — the production `PendingAssembly`, whose settle half
    /// the test holds — so "`reset` returned while the stop was still running"
    /// is a fact of ordering, not of how long anything took. Red against both
    /// halves of the old trade-off: a blocked stop never returns from `reset`
    /// (the gate is released only after it), and a spawned stop with the erase
    /// run alongside it wipes the namespace before the gate opens.
    #[tokio::test]
    async fn a_sign_out_hands_the_loop_back_and_the_erase_waits_for_the_stop() {
        use fauna_client_accounts::{AccountRegistry, SecretStore};
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_credentials(&tx, test_credentials());
        let store = Arc::clone(&app.credentials);
        AccountRegistry::new(Arc::clone(&store) as Arc<dyn SecretStore>)
            .add_account(&"a".repeat(64), Some("https://a.example"), None)
            .expect("the actor registers");
        // A sign-out landing mid-assembly: the stop has to wait for it.
        let (settle, pending) = fauna_client_account_runtime::assembly_channel();
        app.settings.account_store_assembly = Some(pending);

        app.reset();
        assert!(
            app.teardown_pending(),
            "the reset returned with its stop in flight"
        );
        assert!(
            store.get("fauna/index").is_some(),
            "the erase ran while the account runtime was still stopping"
        );
        // A second teardown landing mid-stop finds nothing to stop — and must
        // still not run underneath the first one's store.
        let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::<&str>::new()));
        let second = std::rc::Rc::clone(&order);
        app.after_stops(move |app| {
            assert!(
                app.credentials.get("fauna/index").is_none(),
                "the first sign-out's erase runs before what was queued after it"
            );
            second.borrow_mut().push("second teardown");
        });
        assert!(order.borrow().is_empty(), "queued work ran mid-stop");

        // The assembly settles (failed: nothing to stop), the stop ends, and
        // its completion comes back through the UI queue as the loop's would.
        settle.failed();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(msg) = rx.recv().await {
                let done = matches!(msg, UiMessage::Data(DataMessage::AccountRuntimeStopped));
                app.handle_message(msg);
                if done {
                    break;
                }
            }
        })
        .await
        .expect("the stop reports back through the UI queue");

        assert!(!app.teardown_pending());
        assert_eq!(
            store.get("fauna/index"),
            None,
            "the erase ran after the stop"
        );
        assert_eq!(*order.borrow(), ["second teardown"]);
        assert!(
            matches!(app.launch, crate::launch::LaunchSurface::Wizard),
            "the reset's wizard comes up with the erase, after the stop"
        );
    }

    #[test]
    fn a_test_app_can_never_touch_a_real_keyring() {
        // The guard on `reset_clears_the_whole_credential_namespace` and on every
        // other test that reaches the store: `test_app`'s namespace is a private
        // tmpdir, so `App::reset()`'s wholesale wipe can never sweep the
        // developer's real `fauna-tui` libsecret items. `App::new` would.
        let app = test_app();
        let dir = app
            .credentials
            .file_backend_dir()
            .expect("test_app must use the file backend, never libsecret");
        assert!(dir.starts_with(std::env::temp_dir()));
    }

    // ── Reconnect / push fan-out classification (transport.md § Push events) ──
    //
    // The classification itself — which surfaces a reconnect or a given push
    // kind stales — moved to `fauna_protocol::push_events`, where one
    // exhaustive match serves all seven apps and its own tier_1 tests cover
    // every kind plus two properties tui never had: that a reconnect's sweep
    // covers every kind's set, and that `ResyncRequired`'s differs from it by
    // exactly the feed. What stays here is the one decision that is tui's own.

    /// tui reads pending knocks and the roster in one op, so a push that stales
    /// either one means exactly one refresh. The shared vocabulary keeps them
    /// apart because linux re-reads them independently — folding them is tui's
    /// call, not the seam's.
    #[test]
    fn a_knock_and_a_roster_change_both_reach_tuis_one_contacts_op() {
        use fauna_protocol::push_events::{KnockPayload, StaleSurfaces};

        let knock = PushEvent::Knock(KnockPayload {
            sender_id: "ab".repeat(32),
            summary: "wants to connect".into(),
            ..Default::default()
        });
        assert!(
            contacts_stale(&knock.invalidates()),
            "a knock must reach the contacts op — tui serves the knock list from it"
        );
        assert!(
            contacts_stale(&StaleSurfaces {
                contacts: true,
                ..StaleSurfaces::NONE
            }),
            "a roster change must reach the same op"
        );
        assert!(
            !contacts_stale(&StaleSurfaces {
                account: true,
                ..StaleSurfaces::NONE
            }),
            "an unrelated surface must not drag the contacts op along"
        );
    }

    /// A mouse click on a sidebar tab — `App::click_sidebar` — does exactly
    /// what Enter on that same tab does (`arrows_move_within_a_zone_and_left_right_switch_zones`
    /// above): select AND step into the page in one motion, since a click has
    /// no separate "highlight, then confirm" phase the way arrow keys do.
    #[test]
    fn click_sidebar_navigates_and_enters_the_page_zone() {
        let mut app = authed_app();
        let index = app
            .sidebar_pages()
            .iter()
            .position(|p| *p == Page::Conversations)
            .expect("Conversations is always a sidebar row");
        app.click_sidebar(index);
        assert_eq!(app.page, Page::Conversations);
        assert_eq!(
            app.zone,
            Zone::Page,
            "a click both selects and commits, unlike the keyboard ring"
        );
    }

    /// A hit-test against a frame that has since scrolled/changed can hand
    /// back a stale, out-of-range index — it must be a no-op, not a panic.
    #[test]
    fn click_sidebar_out_of_range_is_a_no_op() {
        let mut app = authed_app();
        let page_before = app.page;
        let zone_before = app.zone;
        app.click_sidebar(9999);
        assert_eq!(app.page, page_before);
        assert_eq!(app.zone, zone_before);
    }

    /// Live-user report: "returning from Folders does NOT return to Settings —
    /// it stays at Folders forever." Root cause: `App::apply`'s `edge` check
    /// (`self.page != page`) is meant to reset `settings.sub` to `Root` whenever
    /// Settings is (re-)entered, but `click_sidebar` mutates `self.page` DIRECTLY
    /// before `apply` ever runs — by the time `apply` checks `edge` it always
    /// reads `false`, so leaving Folders via the sidebar and clicking back into
    /// Settings must still land on the rail Root, not wherever the sub-page was
    /// left.
    #[test]
    fn returning_to_settings_via_the_sidebar_lands_on_the_rail_root() {
        let mut app = authed_app();
        let settings_index = app
            .sidebar_pages()
            .iter()
            .position(|p| *p == Page::Settings)
            .expect("Settings is always a sidebar row");
        let other_index = app
            .sidebar_pages()
            .iter()
            .position(|p| *p == Page::Feed)
            .expect("Feed is always a sidebar row");

        // Enter Settings, drill into Folders.
        app.click_sidebar(settings_index);
        app.settings.sub = crate::settings::SubPage::Folders;

        // Leave via the sidebar to a different page, then return.
        app.click_sidebar(other_index);
        app.click_sidebar(settings_index);

        assert_eq!(
            app.settings.sub,
            crate::settings::SubPage::Root,
            "re-entering Settings via the sidebar must always land on the rail \
             Root, not whatever sub-page was open on the last visit"
        );
    }

    /// The positive case symmetric with `enter_on_a_disabled_button_is_inert`:
    /// clicking an ENABLED button moves the ring there and actuates it through
    /// the exact same `actuate_focused` dispatch Enter uses — proven via the
    /// same observable `dispatch_wizard`'s Ok arm leaves behind (clearing a
    /// stale wizard error), so this is checking actuation, not just focus.
    #[tokio::test]
    async fn click_page_element_moves_focus_and_actuates_the_target() {
        use fauna_onboarding_machine::OnboardingStep;

        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        // Land the ring on `handle-input` (index 0) and type a handle, so
        // `handle-check-button` is enabled — an empty handle would fall into
        // the disabled-guard no-op instead, like `enter_on_a_disabled_button_is_inert`.
        for c in "ada".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let elements = app.page_elements();
        let index = elements
            .iter()
            .position(|e| e.id == "handle-check-button")
            .expect("Check button must be present on HandleEntry");
        assert!(elements[index].enabled, "a non-empty handle enables Check");

        app.wizard.error = Some("stale".to_string());
        app.click_page_element(index);

        assert_eq!(app.zone, Zone::Page);
        assert_eq!(
            app.focused().map(|e| e.id).as_deref(),
            Some("handle-check-button"),
            "the click moved the ring onto the clicked element, like Enter would"
        );
        assert!(
            app.wizard.error.is_none(),
            "the click actuated the button (dispatch_wizard's Ok arm ran), not just focused it"
        );
    }

    /// A label is never in the keyboard ring either, so a click on one must be
    /// as inert as it would be if the keyboard could somehow land there.
    #[test]
    fn click_page_element_on_a_label_is_a_no_op() {
        use fauna_onboarding_machine::OnboardingStep;

        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        let elements = app.page_elements();
        let index = elements
            .iter()
            .position(|e| e.id == "handle-message-area")
            .expect("the message label must be present");
        assert!(!elements[index].focusable());
        let focus_before = app.focus;
        let zone_before = app.zone;
        app.click_page_element(index);
        assert_eq!(
            app.focus, focus_before,
            "a label click must not move the ring"
        );
        assert_eq!(app.zone, zone_before);
    }

    /// Same no-panic contract as the sidebar's out-of-range case.
    #[test]
    fn click_page_element_out_of_range_is_a_no_op() {
        let mut app = test_app();
        let focus_before = app.focus;
        app.click_page_element(9999);
        assert_eq!(app.focus, focus_before);
    }

    fn consent_route() -> fauna_core::app_route::AppRoute {
        fauna_core::app_route::AppRoute::Consent {
            request_uri: "urn:ietf:params:oauth:request_uri:abc_DEF-123".into(),
        }
    }

    /// A route arriving signed out is held — never applied during the launch
    /// flow — and applied through the same door once a session owns the
    /// screen (`apps/tui.md` § System integration → *In-app routes*).
    #[test]
    fn a_route_arriving_signed_out_is_held_until_sign_in() {
        let mut app = test_app();
        assert!(app.apply_route(consent_route()).is_none());
        assert_eq!(app.pending_route, Some(consent_route()));
        assert!(app.apply_pending_route().is_none(), "still signed out");
        assert_eq!(app.pending_route, Some(consent_route()));

        app.session = Some(test_session());
        let _ = app.apply_pending_route();
        assert_eq!(app.pending_route, None, "applied once, then gone");
        assert_eq!(app.page, Page::Settings);
        assert_eq!(app.settings.sub, crate::settings::SubPage::ConnectedApps);
    }

    /// A share route has no producer on tui: dropped, nothing held.
    #[test]
    fn a_share_route_is_dropped() {
        let mut app = authed_app();
        let page = app.page;
        let route = fauna_core::app_route::AppRoute::FolderShare { folder_id: 1 };
        assert!(app.apply_route(route).is_none());
        assert_eq!(app.pending_route, None);
        assert_eq!(app.page, page);
    }
}

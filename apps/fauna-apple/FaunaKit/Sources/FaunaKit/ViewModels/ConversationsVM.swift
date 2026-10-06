import SwiftUI

/// Thin SwiftUI-friendly proxy over `ConversationsManager` (UniFFI). All
/// thread / message / compose state and per-rail logic live in the shared
/// Rust crate `libs/fauna-conversations`; this class:
///
///   1. owns the manager instance (one per app),
///   2. implements `SnapshotObserver` (via `ConversationsObserverBox`) to
///      translate manager notifications into `@Observable` invalidations on
///      the main actor — mirrors `OnboardingVM`'s observer trampoline,
///   3. exposes convenience getters so SwiftUI views read `vm.snapshot` /
///      `vm.detail(id)` / `vm.threads` instead of `vm.manager.snapshot()`
///      everywhere — they're equivalent, but the getters touch the
///      observer-tick so `@Observable` re-renders on change,
///   4. forwards every mutator to the manager. No client-side state machine;
///      no rail branches (see `docs/goal/ui/conversations.md` "Architectural
///      rules"). The markdown-toolbar text-wrap is pure client glue and lives
///      in the views, not here.
///
/// Shared by the macOS and iOS apps; identical behaviour on both. The macOS
/// `TestAgent` (`FaunaMacApp.swift`) reaches `vm.manager` directly for the
/// E2E test-helper calls (`injectInboundForTest`, `createMlsGroup`,
/// `clearForTest`).
@MainActor @Observable
public final class ConversationsVM {
    /// The UniFFI-generated wrapper around the Rust manager. SwiftUI reads
    /// snapshots via the convenience getters below; the TestAgent calls
    /// test-helper methods on it directly. Starts as a **bare** manager (no rail
    /// backends in production) and is replaced by the dual-rail
    /// `ConversationsSession.manager()` on `activate(session:)` at login — that is
    /// what makes Send issue a real RPC.
    public private(set) var manager: ConversationsManager

    /// The logged-in dual-rail session (FaunaMls + SMTP) once `activate` runs.
    /// `nil` before login / in E2E mock mode. `activate` starts its shared-Rust
    /// `startReceiveLoop()`, so inbound MLS DMs and mail flow into `manager` on
    /// their own; the session is held for direct receive-driver access
    /// (`ingestWelcome` / `pollConversations` / `pollMail`) and test helpers.
    public private(set) var session: ConversationsSession?

    private let observerBox: ConversationsObserverBox

    /// The running-app new-message OS banner (`conversations` outcome 11), ticked
    /// from `onManagerChanged` below and reset by `deactivate()`. Held here rather
    /// than by each shell because both would otherwise wire an identical one
    /// (priority #1/#2): this VM is already the app-lifetime owner of the manager
    /// *and* of the only observer that sees every snapshot change. See
    /// ``MessageBannerObserver`` for why there is no second observer and no
    /// frontmost-window rule.
    private let messageBanners = MessageBannerObserver()

    // ── Draft persistence v2 (reserved-folders.md § Drafts Sync) ───────────────
    //
    // The apple twin of android `ConversationsManagerHost.startDraftsSync` and
    // linux `conversations/drafts.rs`: pure trigger glue over the shared
    // `FfiDraftsSync` (which owns the seal, the WS-RPC `fauna.drafts.{get,put}`
    // calls, the launch gate, and the last-saved baseline). `activate` hands us the
    // per-session handle at login; we restore the owner's persisted drafts on launch
    // (so the composer reflects drafts left on this or another of the user's
    // devices) and let `onManagerChanged` drive a debounced autosave after compose
    // edits. No interim draft store to retire — apple consumes the shared manager's
    // `DraftStore` directly, like linux + android.
    private var draftsSync: FfiDraftsSync?
    private var draftsSaveTask: Task<Void, Never>?
    // The launch restore's task, held only so a test can await its end — never
    // cancelled, by design (`restoreDraftsOnLaunch`).
    private var draftsRestoreTask: Task<Void, Never>?

    public init() {
        let manager = ConversationsManager()
        // In E2E mode (either driving backend — see `FaunaE2E`), swap in
        // deterministic mock backends so the keying / capability / membership /
        // inject-inbound tests don't need a live SMTP/Bluesky/etc. Mirrors
        // Windows' `ConversationsManagerHost`. MUST include the in-process path
        // (`FAUNA_E2E_AGENT_PORT`): gating only on `FAUNA_E2E_BRIDGE` left the
        // manager with no test backend in-process, so `injectInboundForTest`
        // no-op'd and every conversation thread read back empty.
        // Compile-time gated, not just runtime-gated: `installMockBackendsForTest`
        // is a `test-helpers` UniFFI seam, absent from the production FFI flavor
        // (testing.md § convention 15 — the automation surface is compiled out of
        // release artifacts). `FaunaE2E.isActive` stays as the inner switch
        // *within* a test-capable build. Mirrors android's
        // `ConversationsManagerHost`, whose lone call moved behind the same gate.
        #if DEBUG
        if FaunaE2E.isActive {
            manager.installMockBackendsForTest()
        }
        #endif
        self.manager = manager
        let box = ConversationsObserverBox()
        self.observerBox = box
        manager.addObserver(obs: box)
        box.target = self
    }

    /// Hold the logged-in `ConversationsSession` and start its receive loop.
    ///
    /// The session is built **over this VM's existing manager**
    /// (`APIClient.conversationsSession(manager:…)` →
    /// `FfiNestClient.conversationsSessionOverManager` → shared-Rust
    /// `ConversationsSession::from_manager`), so `session.manager()` **is** our
    /// `manager`: activating registers the real FaunaMls + SMTP rails *onto* the
    /// object we already observe, and Send (`send` / `sendNewThread`) resolves a real
    /// backend from that moment on. Nothing is swapped and nothing is re-observed.
    ///
    /// ⚠ It used to swap: `self.manager = session.manager()` pointed the VM at a
    /// *different* manager the FFI had built internally (`from_parts`), silently
    /// discarding every thread the old one held. In e2e that destroyed the test
    /// agent's injected threads the instant the real session activated — a race, since
    /// activation runs in a detached `Task` — and it reported nothing, because
    /// `ingest_inbound` only errors when a rail has *no* backend; a swap fails
    /// nothing at all. linux never had this: it passes its process-wide manager into
    /// `from_manager` (`conv_backend::start_conversations_session`). Now apple does too.
    /// Called from both the production and the E2E `applySessionPatch` login paths.
    /// Shared by macOS + iOS. `draftsSync` is this session's drafts autosync handle
    /// (`reserved-folders.md` § Drafts Sync); `nil` leaves persistence off.
    public func activate(session: ConversationsSession, draftsSync: FfiDraftsSync? = nil) {
        self.session = session
        // Adopt this session's draft autosync. A re-login within one process
        // rebuilds the connection + manager and re-runs `activate`, so cancel any
        // prior session's pending autosave before swapping in the new handle.
        // (The outgoing session itself is released by `deactivate()` at the
        // identity change, before this ever runs.)
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        self.draftsSync = draftsSync
        onManagerChanged()
        // Start the shared-Rust push-driven receive loop: it subscribes the
        // session's `NestConversationsPush` (welcome.received + channel.message)
        // and a backstop ticker, driving `ingest_welcome` / `poll_conversations`
        // (and, once a mail source is registered in the FFI factory, `poll_mail`)
        // into this manager — so inbound MLS DMs and mail materialize in the
        // snapshot with no further Swift wiring. Fire-and-forget: the loop spawns
        // a detached tokio task and returns. The native twin of Windows'
        // `App.xaml.cs` `StartReceiveLoop()` call (`docs/goal/ui/conversations.md`
        // § Receiving into the conversations view). Shared by macOS + iOS.
        Task { await session.startReceiveLoop() }
        // Restore the owner's persisted drafts (and lift the autosave gate).
        // No-op when no `draftsSync` was passed (E2E / mock mode).
        restoreDraftsOnLaunch()
    }

    /// Release the logged-in conversations session — apple's conversations-session
    /// teardown, the counterpart `activate` had no partner for until now.
    ///
    /// `account-scoping.md` § The scoping taxonomy, corollary 2 binds this: a live
    /// session is account-scoped class-1 state and "has to be dropped at the
    /// identity change itself, keyed on the identity", and its second rule —
    /// "dropping state is only half of it — the background loops that WRITE that
    /// state must be retired by the same drop" — is what the release actually
    /// achieves here. Dropping the last Swift reference drops the shared-Rust
    /// `ConversationsSession`, whose `closed()` watch is exactly the *event* the
    /// receive loop and every rider `select!` on (the 2026-08-27 fleet-wide move
    /// off tick-bound loop exits), so the outgoing actor's receive loop ends now
    /// rather than at some later tick — or, as apple's own `activate` comment used
    /// to concede, never.
    ///
    /// Called from each target's canonical `dropActorScopedState()` beside the
    /// manager's `clearForIdentityChange()`, never hand-listed per teardown site.
    /// The android twin is `ConversationsManagerHost.stopConversationsSession`;
    /// apple was the outlier that had none. Idempotent — a teardown before any
    /// login is a no-op.
    ///
    /// This is **not** what makes a re-login's engine build succeed: shared Rust
    /// hands the conversations-engine role over in the session factory itself
    /// (`ConversationsManager::retire_conversations_engine`), deliberately not
    /// depending on any shell remembering to release first. This releases the
    /// state and stops the loop; that guarantees the role.
    public func deactivate() {
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        draftsSync = nil
        session = nil
        // The outgoing identity's banner seed. `clearForIdentityChange()` (which
        // `ActorScope.dropAppOwnedState` runs immediately before this) wipes the
        // threads but preserves observers, and the shared tracker never re-seeds on
        // its own — so without this the incoming identity's restored threads would
        // each look "new" and toast. See `MessageBannerObserver.resetForIdentityChange`.
        messageBanners.resetForIdentityChange()
    }

    /// Release this VM's predecessor session, build the fresh one, and activate
    /// it — the one shared seam for what all four apple login call sites (macOS
    /// + iOS, production + e2e `realConversations`) used to do as three
    /// identical copy-pasted statements each. `account-scoping.md` § The
    /// scoping taxonomy, corollary 2: a drop hand-listed at each call site rots
    /// — exactly one canonical seam per app, never four.
    ///
    /// Release-before-build, not release-in-the-catch: `deactivate()` poisons
    /// the predecessor (ends its receive loop, drops the shared-Rust handle)
    /// from this call onward regardless of whether the build below then
    /// succeeds, so holding the predecessor past this point is never right
    /// (`account-data-plane.md` § Multi-instance concurrency → *The role is
    /// HANDED OVER in-process* owns the mechanism this releases into). A
    /// build failure now surfaces as this VM having NO session — same
    /// `NotSupported` Send posture a stale-predecessor reference left it in
    /// before, just without the inert leftover reference.
    ///
    /// Throws rather than swallowing: every call site already wraps its own
    /// best-effort `do/catch` around the three statements this replaces, so
    /// the failure handling stays exactly where it was.
    public func rebuild(
        api: APIClient, selfAddress: String, selfSecretHex: String,
        deviceIdHex: String, predecessorBackupKeys: [Data]
    ) async throws {
        deactivate()
        let session = try await api.conversationsSession(
            manager: manager, selfAddress: selfAddress, selfSecretHex: selfSecretHex,
            deviceIdHex: deviceIdHex, predecessorBackupKeys: predecessorBackupKeys
        )
        let drafts = try? await api.draftsSync(rail: "conversations")
        activate(session: session, draftsSync: drafts)
    }

    /// Attach this session's conversation-drafts autosync to the **current**
    /// manager, **without** adopting a session manager (contrast `activate`, which
    /// swaps in the dual-rail `session.manager()`). Production logins reach drafts
    /// persistence through `activate(session:draftsSync:)`; the in-process e2e
    /// `applySessionPatch` path authenticates only and deliberately keeps the
    /// deterministic mock backends from `init` (so `injectInboundForTest` et al.
    /// stay driveable), so it calls this to exercise `reserved-folders.md` § Drafts Sync
    /// against the real nest — drafts are pure `DraftStore` state, independent of
    /// the rail backends. Restores the owner's persisted drafts on attach and lifts
    /// the autosave gate (the no-data-loss gate stays closed until `load` runs),
    /// mirroring `activate`'s drafts half. Shared by macOS + iOS.
    public func attachDraftsSync(_ handle: FfiDraftsSync) {
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        self.draftsSync = handle
        restoreDraftsOnLaunch()
    }

    // ── Observer ────────────────────────────────────────────────────────────
    fileprivate func onManagerChanged() {
        // The convenience getters below read `_observerTick`, so bumping it
        // here re-evaluates them and SwiftUI re-renders. (Matches the
        // OnboardingVM pattern; the manager itself holds the real state.)
        _observerTick &+= 1
        // Every manager notification (incl. a compose-body/subject edit) re-arms
        // the debounced draft autosave — a cheap no-op when no `draftsSync` is
        // attached or the snapshot is unchanged. Mirrors android's
        // `ConversationsManagerHost` observer + linux's `drafts.rs`.
        scheduleDraftsSave()
        // Feed the backup audit loop's freshness comparison (`ui/backups.md` §
        // Audit-alert surface — the load-bearing observation feed): the newest
        // conversation activity this client has actually *displayed* is its own,
        // source-untrusted evidence that data this recent exists, which the audit
        // compares against what each backup destination holds. Monotonic and a
        // no-op on a repeat notification with the same threads (shared
        // `observe_local_record`); mirrors linux `views/conversations/list.rs`'s
        // `threads.iter().map(|t| t.last_activity_ms).max()` choke point exactly
        // — apple's equivalent "conversation list's own render" is this manager
        // observer, since every render reads through it (`_observerTick`).
        if let newest = manager.snapshot().threads.map(\.lastActivityMs).max() {
            _ = backupAuditObserve(statePath: FaunaClient.backupAuditStatePath, lastActivityMs: newest)
        }
        // The home-screen widget's number (apps/common.md § Home-screen widget):
        // this list's own unread total, from the same snapshot every render reads —
        // linux's `sum_unread` → launcher badge, once per snapshot tick. The
        // publisher writes only when the total changed.
        WidgetUnreadPublisher.shared.publish(
            unread: WidgetUnreadPublisher.total(of: manager.snapshot().threads))
        // The new-message OS banner's diff tick (`conversations` outcome 11). Last,
        // and with its own snapshot read: the tick's start barrier must be bumped
        // strictly before the snapshot it diffs, which is what makes the witness's
        // negative assertions sound — see `MessageBannerObserver.tick`.
        messageBanners.tick(manager: manager)
        // The private contact overlay's projection re-emits on this same
        // observer (`contacts.md` § The private overlay). Assign only on a real
        // move, so a name read re-renders when a name may have changed and not
        // on every message.
        let revision = FfiContactOverlays(manager: manager).revision()
        if revision != overlayRevision { overlayRevision = revision }
    }
    private var _observerTick: UInt64 = 0

    // ── The private contact overlay ─────────────────────────────────────────

    /// Moves exactly when a name read through ``contactOverlays`` may have
    /// changed — a Save here, a sibling device's edit, the projection's first
    /// load after a launch.
    public private(set) var overlayRevision: UInt64 = 0

    /// What the viewer calls a person, for the surfaces keyed on that person —
    /// roster row, knock sender, feed and subscription author, Profile header
    /// (`contacts.md` § The private overlay → *Where the nickname paints*).
    /// A pure pass-through to the shared projection: no view resolves a name,
    /// joins a label line or re-derives the roster filter. Member chips and
    /// message senders never read here — their names come from ``snapshot``,
    /// which applies the paint gate.
    ///
    /// Built afresh on every read, which is cheap and is also what registers
    /// the overlay seam for a manager no conversations session serves (plain
    /// e2e). Reading it observes ``overlayRevision``.
    public var contactOverlays: FfiContactOverlays {
        _ = overlayRevision
        return FfiContactOverlays(manager: manager)
    }

    /// What the viewer calls a post's author (`post-author`,
    /// `feed-post-detail-author`): their nickname for the author when one is
    /// set, else the author's public name — a bridged author's display name or
    /// handle as the nest sent it — else the canonical short id. One door for
    /// the shared feed card and both targets' detail panes.
    public func postAuthorLabel(_ post: PostSummary) -> String {
        contactOverlays.peerLabel(
            displayName: post.authorDisplay?.displayName,
            handle: post.authorDisplay?.handle,
            actorId: post.author
        ).primary
    }

    /// One receive pass over both rails — the session's own reconnect /
    /// missed-push backstop (`pollConversations` + `pollMail`), whose ingest ticks
    /// the manager observer above and so republishes the widget's count. What the
    /// iOS widget-refresh `BGAppRefreshTask` runs while the app is suspended
    /// (`apps/ios.md` § Home-screen widget). Best-effort: no session yet is a no-op,
    /// and a failed rail leaves the count as it was.
    ///
    /// Returns the list's unread total once the pass is over — the number the
    /// widget now shows, read off the same snapshot the observer publishes — or
    /// `nil` when there was no session to poll, so the pass's caller can tell a
    /// pass that reached the rails from one that ran nothing.
    @discardableResult
    public func receivePass() async -> Int? {
        guard let session else { return nil }
        do { _ = try await session.pollConversations() } catch {
            logMessage(level: .info, target: "fauna.widget",
                       message: "background conversations poll failed: \(error)")
        }
        do { _ = try await session.pollMail() } catch {
            logMessage(level: .info, target: "fauna.widget",
                       message: "background mail poll failed: \(error)")
        }
        return WidgetUnreadPublisher.total(of: manager.snapshot().threads)
    }

    // ── Draft persistence triggers ─────────────────────────────────────────────
    /// Restore the owner's persisted conversation drafts on launch. Called from
    /// both `activate` and `attachDraftsSync`; `nil` from `load` is first run
    /// (keep the empty store). A restore failure is logged and left non-fatal —
    /// the `FfiDraftsSync` launch gate then stays closed, so a later autosave
    /// can't clobber the unread blob (no-data-loss). The native twin of
    /// android's `startDraftsSync` load / linux's `restore_when_loaded`.
    ///
    /// The identity epoch is read **here, synchronously, before the `Task` is
    /// spawned** — not inside it after `sync.load()` resolves — and handed to
    /// `restoreDraftsAt`, which refuses the fill once the epoch has moved. A
    /// launch restore the outgoing account started otherwise spawns an
    /// uncancelled `Task` that `deactivate()` never cancels, so it can fill the
    /// manager the incoming account uses (`account-scoping.md` § The scoping
    /// taxonomy). Reading the epoch inside the `Task` after the load would defeat
    /// this: by the time the load resolves the epoch may already have moved.
    private func restoreDraftsOnLaunch() {
        guard let sync = draftsSync else { return }
        let manager = self.manager
        let epoch = manager.identityEpoch()
        draftsRestoreTask = Task { @MainActor in
            do {
                if let bytes = try await sync.load() {
                    // `await`: a restore owes a restored recipient its probe
                    // (`ConversationsManager::restore_drafts_at`), so the shared
                    // manager finishes the rail round-trip before returning.
                    await manager.restoreDraftsAt(epoch: epoch, bytes: bytes)
                }
            } catch {
                logMessage(level: .warn, target: "fauna.conversations.drafts",
                           message: "[drafts] restore on launch failed (non-fatal): \(error)")
            }
        }
    }

    /// Test seam: resolves once the most recent launch restore has finished —
    /// filled, refused on a moved epoch, or failed. A refused restore changes
    /// nothing observable, so without this a test cannot tell "refused" from "not
    /// finished yet" (the epoch-before-load pin,
    /// `ConversationsDraftsRestoreEpochTests`).
    func draftsRestoreSettledForTesting() async {
        await draftsRestoreTask?.value
    }

    /// Debounced persist after a compose change: each manager notification re-arms
    /// the `draftsSaveDebounce` timer and only the last surviving task fires, so a
    /// burst of keystrokes coalesces into one upload. `saveIfChanged` then no-ops
    /// before the launch restore completes (the no-data-loss gate) and for an
    /// unchanged snapshot, so a tick fired by a non-draft manager change costs only
    /// a cheap snapshot compare. A no-op when no `draftsSync` is attached
    /// (pre-login / E2E). Mirrors android's `scheduleDraftsSave` and, on apple,
    /// `FeedVM`'s own (`scheduleDraftsAutosave`, `ViewModels/DraftsAutosave.swift`).
    private func scheduleDraftsSave() {
        guard let sync = draftsSync else { return }
        draftsSaveTask?.cancel()
        let manager = self.manager
        draftsSaveTask = scheduleDraftsAutosave(sync: sync, logTarget: "fauna.conversations.drafts") {
            manager.draftsSnapshotBytes()
        }
    }

    /// Flush the pending autosave immediately — the leave-flush promise
    /// (`reserved-folders.md` § The leave-flush promise): a leave door must not
    /// lose the debounce window's last edit. Called from each target's leave-door
    /// observer — macOS's `AppDelegate.applicationShouldTerminate` bounded quit
    /// gate, iOS's `applicationDidEnterBackground` background-task extension —
    /// which reach this VM through `AppState.conversationsVM`. The events rail's
    /// twin is `EventsVM.flushDraftsNow`; `FeedVM` carries the mirror of this one.
    ///
    /// Cancels the debounced task first so the two cannot both fire, then runs
    /// the save it would have run. A no-op when no `draftsSync` is attached
    /// (pre-login / E2E), which is what lets the leave doors call it
    /// unconditionally.
    public func flushDraftsNow() async {
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        guard let sync = draftsSync else { return }
        let manager = self.manager
        await flushDraftsAutosave(sync: sync, logTarget: "fauna.conversations.drafts") {
            manager.draftsSnapshotBytes()
        }
    }

    /// Quiescence window before an edited draft set is persisted — long enough that
    /// ordinary typing coalesces into one upload, short enough that a draft is safe
    /// within a couple seconds of a pause. Read fresh from
    /// `fauna_client_drafts::AUTOSAVE_DEBOUNCE` via the shared `autosaveDebounceMs()`
    /// FFI face (`reserved-folders.md` § Drafts Sync) — apple holds no literal of
    /// its own. Shared with `FeedVM`/`EventsVM` (all three rails use the same
    /// window) — internal, not `private`, so it stays one apple-wide accessor
    /// rather than growing a second or third copy.
    static var draftsSaveDebounce: Duration { .milliseconds(autosaveDebounceMs()) }

    // ── Convenience getters (read freshly on every access) ─────────────────
    public var snapshot: ConversationsSnapshot {
        _ = _observerTick
        return manager.snapshot()
    }
    public func detail(_ id: ThreadId) -> ThreadDetail? {
        _ = _observerTick
        return manager.threadDetail(id: id)
    }
    public var threads: [ThreadSummary] {
        _ = _observerTick
        return manager.snapshot().threads
    }
    public var selectedThreadId: ThreadId? {
        _ = _observerTick
        return manager.snapshot().selectedThreadId
    }
    public var selectedDetail: ThreadDetail? {
        _ = _observerTick
        guard let id = manager.snapshot().selectedThreadId else { return nil }
        return manager.threadDetail(id: id)
    }
    public var newThreadCompose: ComposeState? {
        _ = _observerTick
        return manager.snapshot().newThreadCompose
    }
    public var addParticipant: AddParticipantState? {
        _ = _observerTick
        return manager.snapshot().addParticipant
    }

    /// The conversations page's `error-message` text (`conversations.md` §
    /// Errors & edge cases — through the fifth truth added 2026-09-15). Reads
    /// `manager.engineServedElsewhere()` FIRST — the conversations-engine
    /// role-lock refusal (`account-data-plane.md` § Multi-instance
    /// concurrency, W5.6 (account-data-plane.md § Workstreams)): a STANDING condition set at the engine-construction
    /// site, never by a page producer, so it must outrank the truths
    /// below rather than being masked by an unrelated gesture clearing them.
    /// Then `manager.receiveStopped()` — the receive loop died by panic
    /// (`ConversationsManager::receive_stopped`, standing exactly like the
    /// served-elsewhere refusal until a newer loop over the same manager
    /// retires it; no gesture clears it either). Then `snapshot.error` — the
    /// membership/label wire-op failure (`confirm_add_participant` /
    /// `remove_participant` / `rename_thread`) — falling back to the
    /// selected thread's compose `send_state`. Those two truths never
    /// overlap and the page error is always the more recent of the two by
    /// construction (every producer clears it on entry, `send` /
    /// `send_new_thread` included), so their precedence is safe and must
    /// not be reversed. Last, the floor: `manager.unopenableMailCount()` —
    /// received mail this run could not open under the account's complete
    /// key set (`mail-app-surface.md` § Inbound client receive → *Unopenable
    /// records*), ranked below every truth above so a fresh failure of any
    /// gesture, a dead rail, or the role refusal all outrank it; no gesture
    /// clears it, only the records opening after all. Mirrors tui's
    /// `sync_page_error` (the reference implementation — read it before
    /// changing this). Shared by macOS + iOS so the two platforms can't
    /// drift on the read.
    public var pageError: String {
        _ = _observerTick
        if manager.engineServedElsewhere() { return L.conversations.errors.servedElsewhere }
        if manager.receiveStopped() { return L.conversations.errors.receiveStopped }
        let snap = manager.snapshot()
        if let pageError = snap.error { return renderLocalizedText(pageError) }
        if case let .failed(reason)? = selectedDetail?.compose.sendState { return renderLocalizedText(reason) }
        let unopenable = manager.unopenableMailCount()
        if unopenable > 0 { return L.conversations.errors.mailUnopenable(count: String(unopenable)) }
        return ""
    }

    // ── Reply preview ──────────────────────────────────────────────────────
    /// What `dm-reply-preview` says about the reply in progress — the shared
    /// `ConversationsManager::reply_preview` record (the answered message's
    /// sender and a plain-text excerpt), `nil` when no reply is armed or the
    /// answered message is outside the fetched window. Apps render the record,
    /// never derive it (`conversations.md` § Where logic lives → *Reply preview*).
    public func replyPreview(_ threadId: ThreadId) -> ReplyPreview? {
        manager.replyPreview(id: threadId)
    }

    // ── Attachments ────────────────────────────────────────────────────────
    /// The shared attachment-byte loader (conversations.md § Attachments).
    /// Resolves a rendered `dm-attachment-image[i]` / `dm-attachment-file[i]`'s
    /// `blobHash` to its plaintext bytes via the manager's in-memory attachment
    /// store (populated by the inbound MIME parse / the send echo for
    /// nest-backed rails). `nil` for an unknown / not-yet-fetched hash (e.g. a
    /// FaunaMls nest blob the client must still GET + decrypt — a follow-on).
    public func attachmentBytes(_ blobHash: String) -> Data? {
        manager.attachmentBytes(blobHash: blobHash)
    }

    /// Whether each of `message`'s attachments has resident bytes right now, in
    /// document order — the shared read-only `attachment_resident` peek, which
    /// neither copies bytes nor marks a miss wanted. Evicting bytes or fetching
    /// them again changes no message, so a bubble keyed on the message alone
    /// keeps painting what it painted before; `ThreadDetailView` hands this to
    /// `DmMessageBubble` as a compared prop, the apple twin of linux's residency
    /// change check (`conversations.md` § Attachments → *Retention*). Touches
    /// the observer tick, so the thread re-renders on the manager's `notify`.
    public func attachmentResidency(_ message: MessageSnapshot) -> [Bool] {
        _ = _observerTick
        return documentAttachments(message.document).compactMap { block in
            guard case let .attachment(blobHash, _, _, _, _, _) = block else { return nil }
            return manager.attachmentResident(blobHash: blobHash)
        }
    }

    /// Reveal a message's blocked remote images (D3 — render-model.md § D3). The manager flips its
    /// in-memory per-message reveal set and re-emits the thread detail with
    /// `RemoteImage.revealed = true`; the bubble repaints off the fresh snapshot (no client-side
    /// reveal state). In-memory only — the no-persistence posture (html-mail.md § Rendering) holds.
    public func revealRemoteImages(_ messageId: MessageId) {
        manager.revealRemoteImages(messageId: messageId)
    }

    // ── Link previews (D4 — render-model.md § D4) ────────────────────────────
    /// Resolve a `LinkPreview` block in an open thread: the manager fetches preview metadata once
    /// via `fauna.linkpreview.resolve` (cached per url) and folds `Resolved`/`Failed` onto the
    /// message's document; the bubble paints when the snapshot notifies. The conversations twin of
    /// `FeedVM.resolveLinkPreview`; a message body that is a bare url already carries the producer's
    /// `LinkPreview { Resolving }` block, so this only drives it terminal.
    public func resolveLinkPreview(_ url: String) async { await manager.resolveLinkPreview(url: url) }

    /// Resolve a link-preview og:image `image_hash` to its public nest blob URL — the SINGULAR
    /// canonical `GET /api/v1/blob/<hash>` (matching `APIClient.blobUrl` / android / web;
    /// render-model.md § D4). The og:image is a content-addressed blob the nest fetched + stored
    /// server-side, loaded blocked-by-default (the bubble gates on `revealed`); `nil` before the
    /// nest url is known. Unlike `FeedVM` (which holds an `APIClient`), this VM works offline with
    /// mock backends and has no nest handle, so it reads the nest base url from the served
    /// account's session material (`FaunaAccounts.sessionMaterial`) — written at login on both the
    /// production and in-process-e2e (`applySessionPatch`) paths — memoized, since the VM is built
    /// pre-login and an init-time read would miss it.
    public func linkPreviewImageURL(_ hash: String) -> URL? {
        nestBaseUrl()?.appendingPathComponent("api/v1/blob/\(hash)")
    }

    @ObservationIgnored private var cachedNodeUrl: URL?
    private func nestBaseUrl() -> URL? {
        if let cachedNodeUrl { return cachedNodeUrl }
        guard let raw = FaunaAccounts.sessionMaterial()?.nestUrl, let url = URL(string: raw) else { return nil }
        cachedNodeUrl = url
        return url
    }

    // ── Reactions / message delete (conversations.md § Reactions & message delete) ──
    /// Toggle `emoji` on `msgId` in thread `id` (`dm-reaction-option` /
    /// `dm-reaction-pill`). The shared manager resolves Add vs Remove against self's
    /// current state, optimistically updates the aggregate, posts a `Reaction` channel
    /// message, and re-emits — the observer repaints, so there's no local optimistic
    /// flip (mirrors windows `ToggleReactionAsync`, linux `toggle_reaction`). The
    /// async op is fire-and-forget from the bubble (`Task { await … }`); FaunaMls-only,
    /// gated client-side on `supportsReactions`.
    public func toggleReaction(_ id: ThreadId, _ msgId: MessageId, _ emoji: String) async {
        await manager.toggleReaction(thread: id, message: msgId, emoji: emoji)
    }
    /// Delete `msgId` in thread `id` (`dm-message-delete-confirm-button`). Sender-only
    /// — the manager rejects a non-own target (the security floor is also enforced on
    /// ingest). It optimistically marks the target deleted + posts a `Delete`; the
    /// observer repaints the `dm-message-deleted` tombstone. FaunaMls-only, gated
    /// client-side on `supportsMessageDelete && isOwn`.
    public func deleteMessage(_ id: ThreadId, _ msgId: MessageId) async {
        await manager.deleteMessage(thread: id, message: msgId)
    }

    // ── Selection / list ───────────────────────────────────────────────────
    public func selectThread(_ id: ThreadId) { manager.selectThread(id: id) }
    /// Select `threadId` AND mark `messageId` as the selected message within
    /// it — the mail-search deep link's target (`conversations.md` § The
    /// selected message; `search.md`'s `SearchNav::Mail` arm). `selectedDetail`
    /// resolves `ThreadDetail.selectedMessageId` read-time, `nil` unless the id
    /// names a message in the currently-loaded thread.
    public func selectThreadAndMessage(_ threadId: ThreadId, _ messageId: MessageId) {
        manager.selectThreadAndMessage(threadId: threadId, messageId: messageId)
    }
    public func clearSelection() { manager.clearSelection() }
    public func startNewConversation() { manager.startNewConversation() }
    public func cancelNewConversation() { manager.cancelNewConversation() }
    /// Deactivate the new-thread composer view on a plain back/dismiss WITHOUT
    /// clearing the draft — re-opening `+` (`startNewConversation`) restores it
    /// (conversations.md § Persistence). Only an explicit `cancelNewConversation`
    /// or a successful send discards the half-written message.
    public func deactivateNewConversation() { manager.deactivateNewConversation() }
    public func markRead(_ id: ThreadId) { manager.markRead(id: id) }

    // ── Thread-list search (priority #4 — a LOCAL filter in the manager, not a
    //    nest re-query; contrast feed search — conversations.md § Where logic
    //    lives). The box drives `set_search_query`; `snapshot().threads` is then
    //    filtered by the manager (case-insensitive label + snippet substring), so
    //    every app is a dumb renderer with no client-side `.filter` twin.
    public var searchQuery: String? {
        _ = _observerTick
        return manager.snapshot().searchQuery
    }
    public func setSearchQuery(_ query: String?) { manager.setSearchQuery(query: query) }

    // ── Thread-list sort (`conversation-sort`) ─────────────────────────────
    /// Advance the sort order one step round the shared cycle (latest activity
    /// → oldest first → unread → latest activity — conversations.md § Where
    /// logic lives → Thread-list sort cycle). Clients never enumerate the
    /// orders themselves; `nextSortOrder` (the UniFFI twin of
    /// `fauna_conversations::snapshot::next_sort_order`) owns the one decision,
    /// this just feeds its result straight back to `setSort`. Mirrors linux
    /// `list.rs` / web `+page.svelte`'s `cycleSort()`.
    public func cycleSort() {
        manager.setSort(order: nextSortOrder(current: manager.snapshot().sort))
    }

    // ── Per-thread compose ─────────────────────────────────────────────────
    public func setComposeBody(_ id: ThreadId, _ body: String) { manager.setComposeBody(id: id, body: body) }
    public func setComposeSubject(_ id: ThreadId, _ subject: String) { manager.setComposeSubject(id: id, subject: subject) }
    public func toggleTopic(_ id: ThreadId) { manager.toggleTopic(id: id) }
    public func setReplyTo(_ id: ThreadId, _ msg: MessageId?) { manager.setReplyTo(id: id, msg: msg) }
    public func clearReplyTo(_ id: ThreadId) { manager.setReplyTo(id: id, msg: nil) }

    // ── Reply recipients (mail — `supports_recipient_selection`) ─────────────
    /// Seed a reply draft on `id` to `msgId` (`dm-reply-button` /
    /// `dm-reply-all-button`). `replyAll == false` → the replied message's
    /// sender only; `true` → every thread participant but self. On rails without
    /// recipient selection it just sets `replyTo` (the editable To line stays
    /// hidden). Supersedes `setReplyTo` for the per-message reply controls.
    public func startReply(_ id: ThreadId, _ msgId: MessageId, replyAll: Bool) {
        manager.startReply(id: id, msgId: msgId, replyAll: replyAll)
    }
    /// Append a recipient to the editable reply To line (`dm-reply-recipient-add`).
    /// `text` is format-parsed via `tryParseTypedAddress`; unparseable input is a
    /// no-op (returns false so the field can keep the text for the user to fix).
    @discardableResult public func addReplyRecipient(_ id: ThreadId, _ text: String) -> Bool {
        guard let addr = tryParseTypedAddress(raw: text) else { return false }
        manager.addReplyRecipient(id: id, addr: addr)
        return true
    }
    /// Drop a recipient from the reply To line (`dm-reply-recipient-remove`) —
    /// from *this reply only*; thread history is untouched.
    public func removeReplyRecipient(_ id: ThreadId, _ addr: TypedAddress) {
        manager.removeReplyRecipient(id: id, addr: addr)
    }

    // ── New-thread compose ─────────────────────────────────────────────────
    public func setNewThreadBody(_ body: String) { manager.setNewThreadBody(body: body) }
    public func setNewThreadSubject(_ subject: String?) { manager.setNewThreadSubject(subject: subject) }
    public func setNewThreadRecipientInput(_ text: String) { manager.setNewThreadRecipientInput(text: text) }
    public func acceptNewThreadChip(_ addr: TypedAddress) { manager.acceptNewThreadChip(addr: addr) }

    /// Open the new-thread composer seeded with `actorIdHex` as a Fauna recipient
    /// chip — the shared half of the profile "Start DM" action
    /// (`profile.md` § Where logic lives → Start DM). Mirrors linux `start_dm`'s
    /// `start_new_conversation` + `accept_new_thread_chip`: pure client nav glue,
    /// no wire op (the MLS group bootstraps lazily on first send). The chip's
    /// display handle is the actor_id hex (an OTHER profile carries no cached
    /// handle — the same fallback the profile header uses). The caller then
    /// switches the app to the Conversations page (per-target nav).
    public func startDirectMessage(actorIdHex: String) {
        startNewConversation()
        guard let actorId = Data(hexString: actorIdHex) else { return }
        acceptNewThreadChip(.fauna(handle: actorIdHex, actorId: actorId))
    }

    // ── Add-participant overlay ─────────────────────────────────────────────
    public func openAddParticipant(_ id: ThreadId) { manager.openAddParticipant(id: id) }
    public func setAddParticipantRecipientInput(_ text: String) { manager.setAddParticipantRecipientInput(text: text) }
    public func acceptAddParticipantChip(_ addr: TypedAddress) { manager.acceptAddParticipantChip(addr: addr) }
    public func cancelAddParticipant() { manager.cancelAddParticipant() }
    @discardableResult public func confirmAddParticipant() async -> ThreadId? { await manager.confirmAddParticipant() }
    /// Commit the active picker's current text as a chip. Add-participant
    /// overlay takes priority over new-thread compose (per the manager).
    /// Commits only the address the async probe (`resolveRecipient`) confirmed —
    /// never a format parse of the raw text — so Enter resolves first, then
    /// accepts (the views do `await resolveRecipient(); acceptCurrentRecipientChip()`).
    @discardableResult public func acceptCurrentRecipientChip() -> Bool { manager.acceptCurrentRecipientChip() }
    /// The shared manager's async recipient probe on whichever picker is active.
    /// Typing owes it: the sync input write parks the picker on `.resolving`, and
    /// only this call moves it to a terminal state (`conversations.md` § Errors &
    /// edge cases → *The picker tells the truth*, 2026-08-29).
    public func resolveRecipient() async { await manager.resolveRecipient() }

    // ── Membership / rename ─────────────────────────────────────────────────
    /// On a `FaunaMls` 1:1 this forks a new `MlsGroup` thread and returns its
    /// id; on every other `(rail, flavor)` it adds in place and returns nil.
    @discardableResult public func addParticipant(_ id: ThreadId, _ addr: TypedAddress) -> ThreadId? {
        manager.addParticipant(id: id, addr: addr)
    }
    public func renameThread(_ id: ThreadId, _ newLabel: String) async { await manager.renameThread(id: id, newLabel: newLabel) }
    /// Drop `addr` from `id`'s membership. On a bound FaunaMls group this posts
    /// an MLS Commit that re-keys the group so the removed member cannot follow
    /// forward (no Welcome); other rails are snapshot-only, and mail refuses by
    /// capability (`supports_membership_change == false`).
    ///
    /// Driven by `ThreadHeader.MemberChip`'s tap on a
    /// `supports_membership_change` thread (`ThreadDetailView`'s
    /// `onRemoveMember`) and by the shared `conversations_real_remove` TestAgent
    /// arm — one wrapper for both, so the agent reaches the same manager method
    /// the user's gesture does rather than each shell re-deriving it.
    public func removeParticipant(_ id: ThreadId, _ addr: TypedAddress) async {
        await manager.removeParticipant(id: id, addr: addr)
    }

    /// `room-settings-save-button` — commit every change the room policy
    /// editor staged, one policy commit each, the hand-over last. Returns
    /// whether ALL of them landed: the editor closes only on `true`, and
    /// otherwise stays open with the refusal on the page's `error-message`
    /// (`ui/conversations.md` § Element IDs, the `room_settings` sub-page).
    /// The loop, its order and the stop-at-the-first-refusal rule are the
    /// manager's (`ConversationsManager::apply_room_settings`), shared with
    /// every other app — this is one call, never a per-edit walk here.
    public func applyRoomSettings(_ id: ThreadId, _ edits: [RoomSettingsEdit]) async -> Bool {
        await manager.applyRoomSettings(id: id, edits: edits)
    }

    // ── Attachments (outbound staging) ──────────────────────────────────────
    /// Client-glue error for the page's `error-message` element — set by shell
    /// code whose failure has no manager-snapshot home (an unreadable picked
    /// file, a denied security scope). Mirrors `FeedVM.clientErrorMessage`
    /// (priority #3): the manager's `ComposeState.sendState` cannot carry these,
    /// and reusing `.failed` would claim a send was attempted when none was.
    /// `ThreadDetailView` / `NewThreadComposeForm` render it in the same banner
    /// as `sendState`.
    public var clientErrorMessage: String?

    /// Stage a user-picked file onto `id`'s compose draft (`attachment-button`).
    /// `conversations.md:621`: "Client glue (native file picker) reads the picked
    /// file's bytes → `manager.add_attachment(thread_id, filename, mime_type,
    /// bytes)`." The manager strips privacy metadata, hashes (BLAKE3), caches the
    /// bytes, and stages the light draft; `send` re-resolves them.
    public func attachFile(_ id: ThreadId, at url: URL) throws {
        let (filename, mimeType, bytes) = try readPickedFile(at: url)
        _ = manager.addAttachment(id: id, filename: filename, mimeType: mimeType, bytes: bytes)
    }

    /// New-thread counterpart — stages onto the single-slot new-thread draft.
    public func attachNewThreadFile(at url: URL) throws {
        let (filename, mimeType, bytes) = try readPickedFile(at: url)
        _ = manager.addNewThreadAttachment(filename: filename, mimeType: mimeType, bytes: bytes)
    }

    /// The `compose.file[attachment-button]` e2e state-injection command's
    /// staging logic (macOS `FaunaMacApp`/iOS `FaunaApp` TestAgent, was
    /// byte-identical on both). Prefers the new-thread draft, falls back to
    /// the selected thread, and never silently drops (e2e rule 11): every
    /// path either stages or reports on `error-message`.
    @MainActor
    public func attachComposerFile(atPath path: String) async {
        let url = URL(fileURLWithPath: path)
        do {
            if newThreadCompose != nil {
                try attachNewThreadFile(at: url)
            } else if let tid = selectedThreadId {
                try attachFile(tid, at: url)
            } else {
                AppMessages.error = "compose.file[attachment-button]: no active conversation composer"
                return
            }
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.file[attachment-button]: staged \(path)")
        } catch {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.file[attachment-button] failed: \(String(describing: error))")
            AppMessages.error = "compose.file[attachment-button] failed: \(error.localizedDescription)"
        }
    }

    /// Unstage the attachment at `index` from `id`'s compose draft — the
    /// `dm-compose-attachment-remove` affordance on a staged chip
    /// (`conversations.md:625`: "Remove via `remove_attachment(thread_id,
    /// index)`"). Index is positional over `ComposeState.attachments`, which is
    /// exactly what the chip `ForEach` enumerates, so the two cannot drift.
    public func removeAttachment(_ id: ThreadId, at index: Int) {
        manager.removeAttachment(id: id, index: UInt32(index))
    }

    /// New-thread counterpart — unstages from the new-thread draft.
    public func removeNewThreadAttachment(at index: Int) {
        manager.removeNewThreadAttachment(index: UInt32(index))
    }

    /// Read a picked file into the `(filename, mime, bytes)` triple the shared
    /// seam takes. The MIME comes from `mimeType(forPath:)` (the shared
    /// `content_type_for_filename` catalog) — a native file panel, unlike a
    /// browser's `File.type`, hands back no MIME. Same catalog
    /// `fauna_conversations::compose::guess_mime_type` wraps for linux's
    /// native call, so both apps agree on the same MIME for the same file.
    private func readPickedFile(at url: URL) throws -> (String, String, Data) {
        let bytes = try Data(contentsOf: url)
        let filename = url.lastPathComponent
        let mimeType = mimeType(forPath: url.path)
        return (filename, mimeType, bytes)
    }

    // ── Send ────────────────────────────────────────────────────────────────
    /// Send the composed body on an **existing** thread (`dm-send-button`).
    /// Routes through `ConversationsManager.send` → the resolved rail backend
    /// (SMTP → `fauna.email.send`; FaunaMls → channel post). On failure the
    /// manager stamps `ComposeState.sendState = .failed(reason)`, which
    /// `ThreadDetailView` surfaces via its error banner — so the thrown error is
    /// swallowed here after the snapshot has recorded it (mirrors linux
    /// `detail.rs` `on_send`, which logs + relies on the snapshot).
    public func send(_ id: ThreadId) async {
        do { try await manager.send(id: id) }
        catch { /* reason already on snapshot.sendState; banner renders it */ }
    }

    /// Materialize the new-thread compose into a thread and send it
    /// (`dm-send-button` in new-thread compose). Shared-Rust `send_new_thread`
    /// first flushes a typed-but-uncommitted recipient, then routes through the
    /// resolved rail and selects the new thread. Returns the new thread id, or
    /// `nil` when there was nothing to send / send failed (the materialized
    /// thread keeps a `Failed` draft to retry).
    @discardableResult
    public func sendNewThread() async -> ThreadId? {
        do { return try await manager.sendNewThread() }
        catch { return nil }
    }
}

/// Trampoline conforming to UniFFI's `SnapshotObserver`. The manager takes
/// the observer via `addObserver` after construction, before `ConversationsVM`'s
/// `self` is fully initialized — late-binding via `target` lets us avoid that
/// ordering, the same way `OnboardingVM`'s `ObserverBox` does.
final class ConversationsObserverBox: SnapshotObserver, @unchecked Sendable {
    weak var target: ConversationsVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onManagerChanged() }
    }
}

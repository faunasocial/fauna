import SwiftUI

/// Phase two of archive import on macOS + iOS — the account's archive links
/// (`docs/goal/behavior/archive-import.md` § Linking and merging, ruling 8).
///
/// Everything that decides is shared Rust: the links themselves are the machine
/// crate's `ArchiveLinks`, which the FFI factory builds with the conversations
/// session and registers as its archive-signal sink
/// (`FfiNestClient.archiveLinks()`); each contact's state is its
/// `ArchiveLinkView` and the row's words the crate's `text` composers. This
/// store only drives and caches, the tui `archive_glue` / `archive_links_tick`
/// shape:
///
/// - ``start(api:)`` — called by the shared login seam
///   (`ConversationsVM.rebuild`) once the session is built: one `driveLinks()`
///   now, then a one-second tick that drives again whenever `linkWorkDue()`
///   reads true (the sink never sends — it runs inside the receive loop) and
///   re-reads the views whenever `linkCaptures()` moved;
/// - ``refresh()`` — the Contacts page re-reads the views with its roster;
/// - ``answer(contact:platform:yes:)`` / ``unlink(contact:platform:)`` — the
///   row's `contact-archive-link-yes` / `-no` / `contact-archive-unlink`.
///
/// Held by `FaunaClient` — a fresh one per sign-in, so an account switch never
/// paints the outgoing account's links — and stopped at its `shutdown()`.
/// Excised from the Fauna Kids build with the rest of archive import (the kids
/// FFI flavor carries no `archive-import` feature): there it is inert.
@MainActor @Observable
public final class ArchiveLinksStore {
    /// The tick's cadence — tui's `archive_links_tick`.
    static let tickMs: UInt64 = 1000

    #if !FAUNA_KIDS
    /// The latest `linkViews()` read; empty before the first, and for a login
    /// that wired no links.
    public private(set) var views: [ArchiveLinkView] = []

    @ObservationIgnored private var links: ArchiveLinks?
    /// The `linkCaptures()` count the views were last re-read at.
    @ObservationIgnored private var seenCaptures: UInt64 = 0
    /// Single-flight guard: one `driveLinks()` at a time (tui's
    /// `archive_links_drive` flag).
    @ObservationIgnored private var driving = false
    #endif
    @ObservationIgnored private var tick: Task<Void, Never>?

    public init() {}

    /// Adopt the links `api`'s conversations session built and start driving
    /// them. Idempotent for the same links; a no-op when the login wired none
    /// (no session yet, or no sync device id).
    public func start(api: APIClient) {
        #if !FAUNA_KIDS
        guard let fresh = api.archiveLinks() else { return }
        if let links, links === fresh { return }
        stop()
        links = fresh
        seenCaptures = fresh.linkCaptures()
        drive(fresh)
        Task { await refresh() }
        tick = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(Self.tickMs))
                guard !Task.isCancelled, let self else { return }
                self.onTick()
            }
        }
        #endif
    }

    /// Stop the tick and drop the links and their views.
    public func stop() {
        tick?.cancel()
        tick = nil
        #if !FAUNA_KIDS
        links = nil
        views = []
        seenCaptures = 0
        driving = false
        #endif
    }

    #if !FAUNA_KIDS
    /// Re-read every contact's link view. Best-effort: a failed read keeps the
    /// previous views rather than blanking a live prompt.
    public func refresh() async {
        guard let links else { return }
        if let fresh = await logTryAsync(.warn, "fauna.archive", "archive links: views", {
            try await links.linkViews()
        }) {
            // A stop or a re-point while the read was in flight owns `views` now.
            if self.links === links { views = fresh }
        }
    }

    /// The row's same-person answer — once, either way.
    public func answer(contact: String, platform: String, yes: Bool) async {
        guard let links else { return }
        _ = await logTryAsync(.warn, "fauna.archive", "archive links: answer") {
            try await links.answerLinkToken(contact: contact, platform: platform, yes: yes)
        }
        await refresh()
    }

    /// Unlink a linked contact: future merges stop, claimed content stays.
    public func unlink(contact: String, platform: String) async {
        guard let links else { return }
        _ = await logTryAsync(.warn, "fauna.archive", "archive links: unlink") {
            try await links.unlinkToken(contact: contact, platform: platform)
        }
        await refresh()
    }

    private func onTick() {
        guard let links else { return }
        if links.linkWorkDue() { drive(links) }
        let captures = links.linkCaptures()
        if captures != seenCaptures {
            seenCaptures = captures
            Task { await refresh() }
        }
    }

    private func drive(_ links: ArchiveLinks) {
        guard !driving else { return }
        driving = true
        Task { [weak self] in
            _ = await logTryAsync(.warn, "fauna.archive", "archive links: drive") {
                try await links.driveLinks()
            }
            guard let self, self.links === links else { return }
            self.driving = false
            await self.refresh()
        }
    }
    #endif
}

#if !FAUNA_KIDS
/// What one Contacts row paints for phase two — `contact-archive-link-prompt`
/// with its yes / no while the pair is asked, `contact-archive-linked` with
/// unlink while linked, nothing in any other state. Carries the row's own
/// contact and platform into the shared calls.
public enum ArchiveLinkRowPaint: Equatable {
    case asked(prompt: String, contact: String, platform: String)
    case linked(label: String, contact: String, platform: String)
}

/// The paints for the roster row of `peerId`, whose shown name is
/// `contactName`: the shared views filtered to that contact (hex, case-blind),
/// worded by the machine crate's composers.
public func archiveLinkRowPaints(
    views: [ArchiveLinkView], peerId: String, contactName: String
) -> [ArchiveLinkRowPaint] {
    views
        .filter { $0.contact.caseInsensitiveCompare(peerId) == .orderedSame }
        .compactMap { view in
            switch view.state {
            case .asked:
                return .asked(prompt: resolveComposedText(archiveLinkPrompt(view: view, contact: contactName)),
                              contact: view.contact, platform: view.platform)
            case .linked:
                return .linked(label: resolveComposedText(archiveLinkedLabel(view: view)),
                               contact: view.contact, platform: view.platform)
            case .offered, .declined, .unlinked:
                return nil
            }
        }
}

/// The row's archive-link affordances, shared by the macOS and iOS Contacts
/// lists. Placed inside the row, under its `.automationScope(Ids.contactRow,
/// index:)`, so each ID registers as `contact-row[i]`'s own.
public struct ContactArchiveLinkAffordances: View {
    let store: ArchiveLinksStore
    let peerId: String
    let contactName: String

    public init(store: ArchiveLinksStore, peerId: String, contactName: String) {
        self.store = store
        self.peerId = peerId
        self.contactName = contactName
    }

    public var body: some View {
        ForEach(Array(archiveLinkRowPaints(views: store.views, peerId: peerId,
                                           contactName: contactName).enumerated()),
                id: \.offset) { _, paint in
            switch paint {
            case let .asked(prompt, contact, platform):
                VStack(alignment: .leading, spacing: 4) {
                    automationText(Ids.contactArchiveLinkPrompt, prompt)
                        .font(.caption)
                    HStack {
                        button(Ids.contactArchiveLinkYes, L.archiveImport.linkYes) {
                            await store.answer(contact: contact, platform: platform, yes: true)
                        }
                        button(Ids.contactArchiveLinkNo, L.archiveImport.linkNo) {
                            await store.answer(contact: contact, platform: platform, yes: false)
                        }
                    }
                }
            case let .linked(label, contact, platform):
                HStack {
                    automationText(Ids.contactArchiveLinked, label)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    button(Ids.contactArchiveUnlink, L.archiveImport.unlink) {
                        await store.unlink(contact: contact, platform: platform)
                    }
                }
            }
        }
    }

    /// Borderless so a tap answers the question rather than opening the row's
    /// profile; the automation Entry runs the same call.
    private func button(_ id: String, _ title: String,
                        _ action: @escaping @MainActor () async -> Void) -> some View {
        Button(title) { Task { await action() } }
            .buttonStyle(.borderless)
            .controlSize(.small)
            .accessibilityIdentifier(id)
            .automationActivate(id) { Task { await action() } }
    }
}
#endif

import Foundation
import SwiftUI

/// The member's side of the nest rules on apple (`nest-rules.md` § The nest
/// content policy → *Carriage*, § Acceptance at joining, and a change of rules)
/// — a paint of the shared `FfiNestRulesBook` (`libs/fauna-ffi/src/nest_rules.rs`
/// over `fauna_client_policy::NestRulesBook`). android's `NestRulesHost`, tui's
/// `nest_rules.rs` and web's `#lib/nestRules.svelte.ts` are the sibling legs.
///
/// Everything that decides — verifying the nest's signature, keeping the
/// last-accepted document per nest, saying whether a read was a change, the rule
/// sets and keyword/labeler factors the engine folds, the summary the settings
/// surface paints — is shared Rust behind the book. What is left here is what the
/// design lets diverge: where the book's records live (the install-scoped
/// `AccountStateDir.base`, like the region record — a nest's signed document is
/// public, never a secret, so a change made while the app was closed is still
/// found by its newer sequence); when to read (login, every reconnect, and every
/// `StaleSurfaces::nest_info` push, which `FaunaClient` re-posts as
/// ``Notification/Name/faunaNestInfoChanged``); and the paint.
///
/// Held by ``RegionStore`` (`RegionStore.nestRules`), which already owns the
/// refresh triggers and the composed render call, so both shells' existing
/// `region.refresh(api:)` calls drive it with no shell edit.
@Observable
public final class NestRulesStore {
    /// Bumped whenever what the book answers may have changed — every surface
    /// that composed a verdict or painted the rules surface reads it.
    public private(set) var revision = 0

    /// The changed notice's nest domain until the member opens the rules
    /// surface; `nil` when there is none.
    public private(set) var changedNotice: String?

    @ObservationIgnored private var book: FfiNestRulesBook?
    @ObservationIgnored private var sessionNestId: String?
    @ObservationIgnored private var sessionDomain: String?
    @ObservationIgnored private var api: APIClient?
    @ObservationIgnored private var observer: NSObjectProtocol?
    /// The read in flight, if any — a caller arriving mid-read awaits it rather
    /// than returning before the session nest is known (a cold app's admin
    /// editor asks while the login read is still out).
    @ObservationIgnored private var inFlight: Task<Void, Never>?

    /// The app's UI language — the nest's reason is shown in it where the admin
    /// wrote one (same constant as the region plane's).
    static let uiLang = RegionStore.uiLang

    public init() {}

    private static var recordURL: URL {
        AccountStateDir.base.appendingPathComponent("nest-rules-book.json")
    }

    /// Open the book on first use, restoring the persisted records (each
    /// verified again) AHEAD of the first read.
    private func openedBook() -> FfiNestRulesBook {
        if let book { return book }
        let opened: FfiNestRulesBook
        if let records = Self.readRecords() {
            opened = FfiNestRulesBook.restore(records: records)
        } else {
            opened = FfiNestRulesBook()
        }
        book = opened
        return opened
    }

    /// Read at login / reconnect, and arm the `nest_info` push observer. A nil
    /// api (the teardown phase of a sign-out or account switch) drops the
    /// session's nest; the book — a record per nest identity, public — stays for
    /// the next sign-in.
    @MainActor
    public func refresh(api: APIClient?) async {
        let book = openedBook()
        guard let api else {
            self.api = nil
            sessionNestId = nil
            sessionDomain = nil
            setNotice(nil)
            revision += 1
            return
        }
        self.api = api
        armObserver()
        await read(book: book, api: api)
    }

    @MainActor
    private func armObserver() {
        guard observer == nil else { return }
        observer = NotificationCenter.default.addObserver(
            forName: .faunaNestInfoChanged, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self, let api = self.api else { return }
                await self.read(book: self.openedBook(), api: api)
            }
        }
    }

    /// Best-effort: a failed read leaves the last-known rules in force and never
    /// throws into the connection; a change persists the book and raises the
    /// notice.
    @MainActor
    private func read(book: FfiNestRulesBook, api: APIClient) async {
        if let inFlight { return await inFlight.value }
        let task = Task { @MainActor in await self.readOnce(book: book, api: api) }
        inFlight = task
        await task.value
        inFlight = nil
    }

    @MainActor
    private func readOnce(book: FfiNestRulesBook, api: APIClient) async {
        defer { revision += 1 }
        do {
            let r = try await api.refreshNestRules(book)
            sessionNestId = r.nestId
            sessionDomain = r.domain
            switch r.read {
            case .changed(let domain, _):
                setNotice(domain)
                persist(book)
            case .first:
                persist(book)
            case .refused, .noRules, .unchanged:
                break
            }
        } catch {
            // The self-description could not be read; the last-known rules stay.
        }
    }

    /// The member opened the rules surface — the notice has done its job.
    @MainActor
    public func dismissNotice() {
        setNotice(nil)
    }

    /// The notice and its test-state mirror move together.
    @MainActor
    private func setNotice(_ domain: String?) {
        changedNotice = domain
        NestRulesTestState.changedNoticeText = domain.map { L.nestRules.changedNotice(nest: $0) }
    }

    /// The session nest's domain as its self-description named it — the
    /// publisher the admin's editor writes under (`FfiRuleEditor.forNest`).
    /// Read now, awaited, when no read has landed yet; `nil` when it cannot be
    /// read.
    @MainActor
    public func domain(api: APIClient) async -> String? {
        if let sessionDomain { return sessionDomain }
        await read(book: openedBook(), api: api)
        return sessionDomain
    }

    /// The book and the session nest it binds, or `nil` before both exist —
    /// what ``RegionStore/render(labels:inputs:subject:)`` passes to the
    /// composed call.
    func source() -> (book: FfiNestRulesBook, nestId: String)? {
        guard let book, let sessionNestId else { return nil }
        return (book, sessionNestId)
    }

    /// What the settings rules surface paints for the session nest; `nil` when
    /// it published none (or before its self-description was read).
    @MainActor
    public func summary() -> NestRulesSummary? {
        _ = revision
        guard let (book, nestId) = source() else { return nil }
        return book.summary(nestId: nestId, lang: Self.uiLang)
    }

    // MARK: - Persistence: the book's signed records, as JSON under the base dir

    private struct Record: Codable {
        var nestId: String
        var domain: String
        var document: Data
        var sequence: UInt64
        var issuedAt: Int64
        var signature: Data
    }

    private func persist(_ book: FfiNestRulesBook) {
        let records = book.stored().map {
            Record(
                nestId: $0.nestId, domain: $0.domain, document: $0.document,
                sequence: $0.sequence, issuedAt: $0.issuedAt, signature: $0.signature)
        }
        guard let data = try? JSONEncoder().encode(records) else { return }
        let url = Self.recordURL
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: url, options: .atomic)
    }

    private static func readRecords() -> [StoredNestRules]? {
        guard let data = try? Data(contentsOf: recordURL),
            let records = try? JSONDecoder().decode([Record].self, from: data)
        else { return nil }
        return records.map {
            StoredNestRules(
                nestId: $0.nestId, domain: $0.domain, document: $0.document,
                sequence: $0.sequence, issuedAt: $0.issuedAt, signature: $0.signature)
        }
    }
}

extension Notification.Name {
    /// `StaleSurfaces::nest_info` — posted by `FaunaClient`'s push observer on
    /// `fauna.nest.info_changed`; ``NestRulesStore`` re-reads the nest's
    /// self-description on it.
    static let faunaNestInfoChanged = Notification.Name("faunaNestInfoChanged")
}

/// The additive `nest_rules_changed_notice` state field
/// (`tests/e2e-unified/tests/test_nest_rules.py`; `nest-rules.md` § Element IDs:
/// the changed notice adds no element id) — the notice's full text while it is
/// up, `nil` once the member opened the rules surface. Mirrors tui's
/// `state["nest_rules_changed_notice"]`.
@MainActor
public enum NestRulesTestState {
    public static var changedNoticeText: String?
}

/// Convention 17's "a nest Block never renders silent" state field
/// (`tests/e2e-unified/helpers/frame_invariants.py`, published as
/// `nest_block_render`) — ``RegionBlockRender``'s twin. `blocked` is the set of
/// on-screen items whose composed verdict a nest rule blocks, registered by each
/// surface's item container from the verdict it computed; `painted` the nest
/// block placeholders actually composed.
@MainActor
public enum NestBlockRender {
    static var blocked: Set<String> = []
    static var painted: Set<String> = []

    public static var stateFragment: [String: Any] {
        ["blocked": blocked.count, "placeholders": painted.count]
    }
}

extension View {
    /// Register this item container's nest verdict-side walk for convention 17
    /// while it is on screen. No-op in production.
    func nestBlockWitness(_ key: String, blocked: Bool) -> some View {
        #if DEBUG
        self
            .onAppear { if blocked { NestBlockRender.blocked.insert(key) } }
            .onDisappear { NestBlockRender.blocked.remove(key) }
            .onChange(of: blocked) { _, now in
                if now { NestBlockRender.blocked.insert(key) } else { NestBlockRender.blocked.remove(key) }
            }
        #else
        self
        #endif
    }
}

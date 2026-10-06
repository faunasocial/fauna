import Foundation

/// One still-open review item: the raw person id (verdict actions need it)
/// plus the shared row-text parts (`APIClient.memberReviewRowText` —
/// `fauna_core::data::review_row_text`, consumed, never re-derived). `text.who`
/// is resolved from whatever handle `APIClient.memberReviewHandleForPerson`
/// found at LOAD time — re-resolving it after a Remove would find no seat left
/// to read one off.
public struct MemberReviewRow: Identifiable {
    public let person: Data
    public let text: MemberReviewRowText
    public var id: Data { person }
}

/// Drives the permanent **Members To Review** Settings sub-page (item (iv) of `succession-aftermath.md` § Propagation's two-surface ruling).
/// Renders whatever a review sweep left unanswered; unlike the (not-yet-built
/// on apple) ephemeral kit-side pass this page has **no sweep gate** — it is
/// reachable, and ordinarily empty, at all times (`docs/goal/ui/settings.md`
/// § Navigation model). Mirrors tui `settings/member_review.rs::review_rows` /
/// android's `MemberReviewVM`; zero shared logic owed here (priority #2) —
/// every seam is `libs/fauna-ffi/src/member_review.rs`.
///
/// **The verdict is DERIVED, never chosen.** `APIClient.memberReviewRemove`
/// always evicts first and persists only what the eviction earned; this VM
/// never constructs a verdict of its own.
@MainActor @Observable
public final class MemberReviewVM {
    /// The open roster, resolved for display, and whether a read has
    /// returned — an empty list means "nothing open" only once `loaded` is
    /// true (`docs/goal/ui/README.md` § *List pages: loading is not empty*).
    public var rows: [MemberReviewRow] = []
    public var loaded = false
    public var errorMessage: String?

    private var api: APIClient?
    /// Refreshes the SHARED roster cache (`FaunaClient.memberReviewRoster`)
    /// that the thread-header chip marks + contacts badge read — this page's
    /// own `rows` is a separate, richer (row-text-resolved) projection, but a
    /// Keep/Remove decided here must still clear the mark those other two
    /// surfaces paint (`succession-aftermath.md` § Propagation: one flag,
    /// rendered on every surface that has a stake in it).
    private var onRosterChanged: (() async -> Void)?

    private static let hydrateAttempts = 10
    private static let hydrateRetryNanoseconds: UInt64 = 500_000_000

    public init() {}

    public func configure(api: APIClient, onRosterChanged: (() async -> Void)? = nil) {
        self.api = api
        self.onRosterChanged = onRosterChanged
    }

    /// Re-read the roster, retrying while the WS-RPC socket comes up (like
    /// the sibling settings pages). A verdict another device recorded must
    /// not be re-asked here, which is why this always re-reads rather than
    /// patching the cached list in place.
    ///
    /// Kept: `memberReviewList` composes a config-store fetch with a local
    /// unseal, not a single NestClient RPC (transport.md § Request lifecycle
    /// step 3's note; mirrors linux's `load_reviews`).
    public func load() async {
        guard let api else { return }
        for attempt in 0..<Self.hydrateAttempts {
            do {
                let reviews = try await api.memberReviewList()
                var built: [MemberReviewRow] = []
                built.reserveCapacity(reviews.count)
                for review in reviews {
                    let handle = await api.memberReviewHandleForPerson(person: review.person)
                    let text = try api.memberReviewRowText(
                        person: review.person, reasons: review.reasons, handle: handle)
                    built.append(MemberReviewRow(person: review.person, text: text))
                }
                rows = built
                loaded = true
                return
            } catch {
                if attempt == Self.hydrateAttempts - 1 {
                    errorMessage = error.localizedDescription
                } else {
                    try? await Task.sleep(nanoseconds: Self.hydrateRetryNanoseconds)
                }
            }
        }
    }

    /// Record **Keep** — closes every open item for `person` with no group
    /// changes; a concurrent device may have already answered, which is a
    /// success no-op, never an error. Re-reads after (the answered row drops
    /// out).
    public func keep(person: Data) async {
        guard let api else { return }
        do {
            _ = try await api.memberReviewKeep(person: person)
            errorMessage = nil
            await load()
            await onRosterChanged?()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Record **Remove** — evicts `person` from every group of the owner's
    /// they are in NOW (re-derived, never from the stored item; the FFI seam
    /// persists only what the eviction earned, so this app cannot record
    /// `Removed` from its own reasoning). Composes this app's own outcome
    /// message from the returned `evicted`/`failed`/`unreachable` counts
    /// (mirrors android's `removeResultMessage`), then re-reads the roster
    /// regardless: a full eviction drops the row, a partial one re-renders
    /// from the unchanged state. `who` is resolved from the cached row BEFORE
    /// the call — there is no seat left to read a handle off afterward.
    public func remove(person: Data) async {
        guard let api else { return }
        let who = rows.first { $0.person == person }.map { renderLocalizedText($0.text.who) }
            ?? L.settings.recoveryKit.reviewUnknownPerson
        do {
            let eviction = try await api.memberReviewRemove(person: person)
            errorMessage = removeResultMessage(eviction: eviction, who: who)
            await load()
            await onRosterChanged?()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// `CrossGroupEviction` → this app's `error-message` text — `nil` when the
/// row is expected to drop out cleanly on the next re-read. The FFI seam only
/// invokes the verdict-persisting write once an eviction is COMPLETE
/// (`failed`/`unreachable` both empty), so a partial eviction here never
/// means a failed persist — only a seat still standing. A free function (not
/// a VM member) so it is directly testable, mirroring android's
/// `removeResultMessage`.
func removeResultMessage(eviction: CrossGroupEviction, who: String) -> String? {
    if eviction.failed.isEmpty && eviction.unreachable.isEmpty { return nil }
    var parts: [String] = []
    if !eviction.failed.isEmpty {
        let groups = eviction.evicted.count + eviction.failed.count
        parts.append(L.settings.recoveryKit.reviewRemovePartial(
            who: who, removed: String(eviction.evicted.count), groups: String(groups)))
    } else if !eviction.evicted.isEmpty {
        parts.append(L.settings.recoveryKit.reviewRemoveDoneHere(
            who: who, removed: String(eviction.evicted.count)))
    } else {
        parts.append(L.settings.recoveryKit.reviewRemoveNoneHere(who: who))
    }
    let folders = eviction.unreachable.filter { $0.`class` == .folderChannel }.count
    if folders > 0 {
        parts.append(L.settings.recoveryKit.reviewRemoveFolderSeats(seats: String(folders)))
    }
    let unsynced = eviction.unreachable.filter { $0.`class` == .chatGroupNoThreadHere }.count
    if unsynced > 0 {
        parts.append(L.settings.recoveryKit.reviewRemoveUnsyncedSeats(seats: String(unsynced)))
    }
    return parts.joined(separator: " ")
}

/// Whether `personHex` (a contact's actor id, hex) carries an open
/// post-succession review item in `roster` — `contact-unattested-mark`'s own
/// gate (`succession-aftermath.md` § Propagation). Re-derives
/// `fauna_core::data::is_under_review` locally by raw BYTE comparison, never
/// handle-keyed (an MLS roster's handles are attacker-chosen by threat
/// model) — that projection carries no UniFFI export by design
/// (`libs/fauna-ffi/src/member_review.rs`'s own doc), so hand-rolling the
/// comparison here, once, is the sanctioned shape (mirrors android's
/// `ContactsScreen.kt` `contentEquals` precedent). A free function, shared by
/// both apple targets' contacts lists so the two cannot drift on it.
public func contactIsUnderReview(personHex: String, roster: [FfiMemberReview]) -> Bool {
    guard let person = Data(hexString: personHex) else { return false }
    return roster.contains { $0.person == person }
}

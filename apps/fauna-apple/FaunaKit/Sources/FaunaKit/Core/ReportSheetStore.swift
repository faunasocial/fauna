import Foundation
import Observation

/// The state behind the one shared report sheet and its acknowledgement line
/// (`report-sheet`, `report-status` — `moderation.md` § User-initiated reporting
/// → *App surface*). One sheet for the three verbs (a feed post's ⋯, a
/// conversation message's ⋯, an OTHER profile), so one store: a verb calls
/// ``open(_:)`` with the target the shared Rust constructors built, and the
/// ``ReportHost`` paints whatever is open.
///
/// Every decision is shared Rust's (`fauna_client_moderation::report` over
/// UniFFI): the reason list, the submit gate, the include-text rule and the
/// words come from ``view``; the request is built from the sheet by the shared
/// `report_request` inside `abuse_report_submit`. This type only sequences the
/// follow-ups, exactly as web's `ReportHost.svelte` and tui's `report.rs` do:
/// submit → `knocks_block(author)` when ticked → the reporter-side hide → the
/// stored list into the render inputs. A failed send keeps the sheet open; a
/// failed block/hide lands on ``errorMessage`` BESIDE the acknowledgement —
/// never silent.
///
/// App-scoped, held by ``ContentPolicyStore`` (the reporter-side hide list is
/// that store's input), so a report filed from a feed card whose body the hide
/// then replaces still has somewhere to paint its acknowledgement.
///
/// Not actor-isolated as a type, for the same reason as the sibling
/// `ContentPolicyStore` that holds it (a stored-property initializer cannot
/// call a `@MainActor` one); every mutating method is `@MainActor` instead.
@Observable
public final class ReportSheetStore {
    /// The subject the opening verb chose; `nil` is a closed sheet.
    public private(set) var target: FfiReportTarget?
    /// The sheet's draft, folded into ``view`` on every keystroke.
    public var form = ReportSheetStore.emptyForm
    public private(set) var sending = false
    /// The acknowledgement — present only after a send landed, until the next open.
    public private(set) var status = ""
    /// The page's `error-message` line (`""` clears it).
    public private(set) var errorMessage = ""

    public init() {}

    static let emptyForm = FfiReportForm(reason: nil, note: "", includeText: false, blockAuthor: false)

    /// The shared per-keystroke fold, `nil` while the sheet is closed.
    public var view: FfiReportSheetView? {
        target.map { reportSheetView(target: $0, form: form) }
    }

    /// Open the sheet on `target` from an empty draft with no stale
    /// acknowledgement or error.
    @MainActor public func open(_ target: FfiReportTarget) {
        form = Self.emptyForm
        status = ""
        errorMessage = ""
        sending = false
        self.target = target
    }

    @MainActor public func cancel() {
        target = nil
    }

    /// Drop everything — the departing account's draft and acknowledgement must
    /// never reach the next one (`account-scoping.md` § The scoping taxonomy).
    @MainActor public func reset() {
        target = nil
        form = Self.emptyForm
        status = ""
        errorMessage = ""
        sending = false
    }

    /// The id the reporter-side hide keys on: a post's or message's record id,
    /// or an account's actor id (`hide_reported`'s contract).
    static func subjectId(of subject: FfiReportSubject) -> String? {
        switch subject {
        case .post(let cid): return cid
        case .message(_, let recordCid): return recordCid
        case .actor(let actorId): return actorId
        case .unknown: return nil
        }
    }

    @MainActor func setError(_ message: String) {
        errorMessage = message
    }

    /// Send the report and run the follow-ups.
    @MainActor public func submit(api: APIClient?, contentPolicy: ContentPolicyStore) async {
        guard let api, let sent = target, view?.canSubmit == true, !sending else { return }
        sending = true
        let draft = form
        let reply: FfiReportSent
        do {
            reply = try await api.moderationClient().abuseReportSubmit(target: sent, form: draft)
        } catch {
            // The sheet stays open for a retry.
            errorMessage = renderLocalizedText(reportFailed(error: String(describing: error)))
            sending = false
            return
        }
        var followup: String?
        if draft.blockAuthor, let author = sent.author {
            do {
                try await api.blockKnock(actorId: "", peerId: author)
            } catch {
                followup = "block: \(error)"
            }
        }
        if let id = Self.subjectId(of: sent.subject) {
            do {
                contentPolicy.setHiddenContent(try await hideReported(id: id))
            } catch {
                followup = followup ?? "hide: \(error)"
            }
        }
        status = renderLocalizedText(reply.acknowledgement)
        errorMessage = followup ?? ""
        sending = false
        target = nil
    }
}

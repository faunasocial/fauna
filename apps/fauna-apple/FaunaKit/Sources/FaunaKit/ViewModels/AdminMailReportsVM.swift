// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the admin **`admin-mail-reports`** page (macOS + iOS,
/// one FaunaKit VM): a thin proxy over the shared `DmarcReportsMachine` (UniFFI,
/// `libs/fauna-client-mail-settings::dmarc_reports`). The machine owns the window
/// read, the fold into totals / shares / top lists and the opened report's gunzip;
/// this VM holds the latest `DmarcReportsSnapshot` as `@Observable` state and
/// re-reads it after each `configure` / `dispatch`. The machine is **pull-based**
/// (no observer callback), so re-assigning `snapshot` drives the SwiftUI re-render.
///
/// Target behavior: `docs/goal/behavior/dmarc-reporting.md` § Admin pane summary
/// surface. Page §/IDs: ui.yaml `admin-mail-reports`. Reference renderer: tui
/// (`apps/fauna-tui/src/admin/mail_reports.rs`).
@MainActor @Observable
public final class AdminMailReportsVM: MachineBackedVM {
    /// Latest snapshot; `nil` until `configure`.
    public internal(set) var snapshot: DmarcReportsSnapshot?
    /// The single page error surface (`error-message`) — both build failures and
    /// the machine's `snapshot.error` route here.
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: DmarcReportsMachine?
    /// The `APIClient` the cached ``machine`` was built from — a session re-point
    /// must re-vend it (same guard as `AdminCalendarVM`).
    private var configuredApi: APIClient?

    public init() {}

    // MARK: - Lifecycle (pull-based; machine built once, hydrate on every appear)

    /// Vend the machine from APIClient and read the default window. Idempotent.
    public func configure(api: APIClient) async {
        if machine == nil || configuredApi !== api {
            do {
                let m = try await api.dmarcReportsMachine()
                machine = m
                configuredApi = api
                snapshot = m.snapshot()
            } catch {
                errorMessage = DisplayError.message(error)
                return
            }
        }
        await hydrate()
    }

    /// Re-read the window (page mount).
    public func hydrate() async { await hydrateFromMachine() }

    // MARK: - Actions

    public func refresh() async { await dispatch(.refresh) }

    /// Pick the window (one of `dmarcReportWindowOptions()`) and re-read.
    public func setWindow(days: UInt32) async { await dispatch(.setWindow(days: days)) }

    /// Pick one local domain, or every domain (`nil`), and re-read.
    public func setDomain(_ domain: String?) async { await dispatch(.setDomain(domain: domain)) }

    /// Open one report's XML into the raw view.
    public func openReport(_ row: DmarcReportRowView) async {
        await dispatch(.openReport(ourDomain: row.ourDomain, submitter: row.submitter,
                                   reportId: row.reportId))
    }

    public func dispatch(_ action: DmarcReportsAction) async { await dispatchToMachine(action) }
}
#endif // !FAUNA_KIDS

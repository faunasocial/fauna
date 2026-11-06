// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The admin **`admin-mail-reports`** page (`docs/goal/behavior/dmarc-reporting.md`
/// § Admin pane summary surface), shared by macOS + iOS (one FaunaKit view, thin
/// per-target mount points). A dumb renderer of `DmarcReportsSnapshot` and
/// dispatcher of `DmarcReportsAction` over the shared `DmarcReportsMachine` (via
/// `AdminMailReportsVM`): the totals, the shares, the merged top lists and the raw
/// view's gunzip are all the machine's; this file only words them. Element IDs
/// match `tests/e2e-unified/ui.yaml` `admin-mail-reports` exactly. Reference
/// renderer: tui (`apps/fauna-tui/src/admin/mail_reports.rs`).
///
/// Before the first read answers (`loaded`), only the heading, the filters and
/// refresh paint — "no reports" and "not read yet" are different facts. The
/// conditional texts (`-truncated`, `-empty`, `-raw`) paint only on their own path.
/// The three indexed components (`-source`, `-sender`, `-report`) scope each row's
/// leaves with `.automationScope`.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// bar (iOS), not this page.
public struct AdminMailReportsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminMailReportsVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.admin.mailReportsPage.title)
                    .font(.title)
                Text(L.admin.mailReportsPage.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                filters
                if let snap = vm.snapshot, snap.loaded {
                    summary(snap)
                    if !snap.sources.isEmpty {
                        countList(
                            title: L.admin.mailReportsPage.sourcesTitle, rows: snap.sources,
                            container: Ids.adminMailReportsSource,
                            valueId: Ids.adminMailReportsSourceIp,
                            countId: Ids.adminMailReportsSourceCount)
                    }
                    if !snap.senders.isEmpty {
                        countList(
                            title: L.admin.mailReportsPage.sendersTitle, rows: snap.senders,
                            container: Ids.adminMailReportsSender,
                            valueId: Ids.adminMailReportsSenderDomain,
                            countId: Ids.adminMailReportsSenderCount)
                    }
                    if !snap.reports.isEmpty { reportList(snap.reports) }
                    if let opened = snap.opened { rawView(opened) }
                } else if vm.snapshot == nil {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    // MARK: - Filters

    private var windowOptions: [String] { dmarcReportWindowOptions().map { String($0) } }

    private var domainOptions: [String] { [""] + (vm.snapshot?.localDomains ?? []) }

    private func domainLabel(_ domain: String) -> String {
        domain.isEmpty ? L.admin.mailReportsPage.everyDomain : domain
    }

    /// The window picker's raw value is the day count; the domain picker's raw
    /// value is the domain, `""` for every domain (ui.yaml `admin-mail-reports`).
    private var filters: some View {
        let window = Binding<String>(
            get: { String(vm.snapshot?.windowDays ?? 7) },
            set: { raw in
                if let days = UInt32(raw) { Task { await vm.setWindow(days: days) } }
            })
        let domain = Binding<String>(
            get: { vm.snapshot?.ourDomain ?? "" },
            set: { raw in Task { await vm.setDomain(raw.isEmpty ? nil : raw) } })
        return GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Picker(L.admin.mailReportsPage.windowLabel, selection: window) {
                    ForEach(windowOptions, id: \.self) { days in
                        Text(L.admin.mailReportsPage.windowDays(days: days)).tag(days)
                    }
                }
                .accessibilityIdentifier(Ids.adminMailReportsWindowSelect)
                .automationSelect(Ids.adminMailReportsWindowSelect,
                                  value: { window.wrappedValue },
                                  options: { windowOptions }) { window.wrappedValue = $0 }

                Picker(L.admin.mailReportsPage.domainLabel, selection: domain) {
                    ForEach(domainOptions, id: \.self) { d in
                        Text(domainLabel(d)).tag(d)
                    }
                }
                .accessibilityIdentifier(Ids.adminMailReportsDomainSelect)
                .automationSelect(Ids.adminMailReportsDomainSelect,
                                  value: { domain.wrappedValue },
                                  options: { domainOptions }) { domain.wrappedValue = $0 }

                HStack {
                    Spacer()
                    Button(L.admin.mailReportsPage.refresh) { Task { await vm.refresh() } }
                        .accessibilityIdentifier(Ids.adminMailReportsRefreshButton)
                        .automationActivate(Ids.adminMailReportsRefreshButton) {
                            Task { await vm.refresh() }
                        }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - Window totals

    private func summary(_ snap: DmarcReportsSnapshot) -> some View {
        let t = snap.totals
        return GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                automationText(Ids.adminMailReportsSummary, L.admin.mailReportsPage.summaryTitle)
                    .font(.headline)
                automationText(
                    Ids.adminMailReportsMessages,
                    L.admin.mailReportsPage.messages(
                        messages: String(t.messageCount), reports: String(t.reportCount),
                        peers: String(t.submitterCount)))
                automationText(
                    Ids.adminMailReportsAlignment,
                    L.admin.mailReportsPage.alignment(
                        dmarc: String(t.dmarcPass), dmarcShare: String(t.dmarcPassPercent),
                        dkim: String(t.dkimPass), dkimShare: String(t.dkimPassPercent),
                        spf: String(t.spfPass), spfShare: String(t.spfPassPercent)))
                automationText(
                    Ids.adminMailReportsDispositions,
                    L.admin.mailReportsPage.dispositions(
                        none: String(t.dispositionNone),
                        quarantine: String(t.dispositionQuarantine),
                        reject: String(t.dispositionReject)))
                if snap.truncated {
                    automationText(Ids.adminMailReportsTruncated, L.admin.mailReportsPage.truncated)
                        .font(.caption).foregroundStyle(.secondary)
                }
                if snap.reports.isEmpty {
                    automationText(Ids.adminMailReportsEmpty, L.admin.mailReportsPage.empty)
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - Top sources / senders (indexed)

    private func countList(
        title: String, rows: [DmarcCountRow], container: String, valueId: String, countId: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            ForEach(Array(rows.enumerated()), id: \.offset) { offset, row in
                HStack {
                    automationText(valueId, row.value)
                    Spacer(minLength: 12)
                    automationText(countId, L.admin.mailReportsPage.rowMessages(count: String(row.count)))
                        .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(container)
                .automationValue(container, text: { "" })
                // Scoped container: the leaves are queried via `scope="<container>[N]/..."`.
                .automationScope(container, index: offset)
            }
        }
    }

    // MARK: - Report rows (indexed)

    private func reportList(_ reports: [DmarcReportRowView]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(L.admin.mailReportsPage.reportsTitle).font(.headline)
            ForEach(Array(reports.enumerated()), id: \.offset) { offset, r in
                VStack(alignment: .leading, spacing: 2) {
                    automationText(Ids.adminMailReportsReportSubmitter, r.submitter)
                    automationText(Ids.adminMailReportsReportDomain, r.ourDomain)
                        .font(.caption).foregroundStyle(.secondary)
                    automationText(
                        Ids.adminMailReportsReportPeriod,
                        L.admin.mailReportsPage.reportPeriod(
                            begin: formatUnixLocalDate(secs: r.beginAt),
                            end: formatUnixLocalDate(secs: r.endAt)))
                        .font(.caption)
                    automationText(
                        Ids.adminMailReportsReportMessages,
                        L.admin.mailReportsPage.rowMessages(count: String(r.messageCount)))
                        .font(.caption)
                    automationText(
                        Ids.adminMailReportsReportPass,
                        L.admin.mailReportsPage.reportPass(
                            passed: String(r.dmarcPass), count: String(r.messageCount)))
                        .font(.caption)
                    Button(L.admin.mailReportsPage.view) { Task { await vm.openReport(r) } }
                        .accessibilityIdentifier(Ids.adminMailReportsReportViewButton)
                        .automationActivate(Ids.adminMailReportsReportViewButton) {
                            Task { await vm.openReport(r) }
                        }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.adminMailReportsReport)
                .automationValue(Ids.adminMailReportsReport, text: { "" })
                .automationScope(Ids.adminMailReportsReport, index: offset)
                Divider()
            }
        }
    }

    // MARK: - Opened report

    private func rawView(_ opened: DmarcOpenedReport) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.admin.mailReportsPage.rawTitle).font(.headline)
            automationText(Ids.adminMailReportsRaw, opened.xml)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
        }
    }
}
#endif // !FAUNA_KIDS

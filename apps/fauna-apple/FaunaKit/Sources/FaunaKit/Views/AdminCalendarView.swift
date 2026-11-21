// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The flat admin **`admin-calendar`** page (`docs/goal/behavior/admin.md` § 8
/// Calendar + `docs/goal/behavior/caldav-server.md` § Independent enablement),
/// shared by macOS + iOS (one FaunaKit view, thin per-target mount points).
/// The CalDAV-enable sibling of `AdminMailView`: a dumb
/// renderer of `CaldavPolicySnapshot` + dispatcher of `CaldavPolicyAction` over
/// the shared `CaldavPolicyMachine` (via `AdminCalendarVM`); no policy logic here.
/// Element IDs match `tests/e2e-unified/ui.yaml` `admin-calendar` exactly.
/// Reference renderer: linux (`apps/fauna-linux/src/settings/admin_calendar.rs`).
///
/// One control: the deployment-wide **CalDAV-enable** toggle
/// (`admin-calendar-enabled-toggle`) — the admin shell is the third setter of
/// `fauna.bridges.set_caldav_enabled` (alongside onboarding + the user
/// mail-settings page). After the write the toggle re-reads persisted state
/// (`fauna.bridges.get_mail_config` → `caldav_enabled`). The MDA bridge runs iff
/// `mail_enabled || caldav_enabled`, so flipping this on starts the CalDAV
/// listener.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// stack (iOS), not this page.
public struct AdminCalendarView: View {
    @State private var vm = AdminCalendarVM()
    /// Edit buffer for the CalDAV-port field — a `String` so it parses on Save with
    /// a u16-range guard (a stray edit never silently writes a bad port). Re-seeded
    /// from `vm.snapshot.caldavPort` whenever the hydrated value changes.
    @State private var caldavPortText = ""
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.adminCalendarHeading, L.admin.calendarPage.title)
                    .font(.title)
                Text(L.admin.calendarPage.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                if let snap = vm.snapshot {
                    enableGroup(snap)
                    portSection
                } else {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .adminReadOnEntryAndReconnect(vm, reloadToken: reloadToken)
        // Re-seed the edit buffer from the hydrated value (initial + after a save's
        // re-read); a live user/driver edit changes the buffer, not the snapshot,
        // so this never clobbers an in-progress edit.
        .onChange(of: vm.snapshot?.caldavPort, initial: true) { _, port in
            if let port { caldavPortText = String(port) }
        }
    }

    // MARK: - Admin-set CalDAV listener port (gathered on Save)

    /// The admin-set CalDAV listener port (`admin-calendar-caldav-port-*`;
    /// caldav-server.md § Network exposure) — a text_input + save button over the
    /// shared `CaldavPolicyMachine`, the symmetric twin of the admin-nest serving
    /// port. Lifted to apple from linux/android to close the priority-#1 parity gap.
    private var portSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline) {
                    Text(L.admin.calendarPage.caldavPortLabel)
                    Spacer(minLength: 12)
                    TextField("", text: $caldavPortText)
                        .multilineTextAlignment(.trailing)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 160)
                        .accessibilityIdentifier(Ids.adminCalendarCaldavPortInput)
                        .automationField(Ids.adminCalendarCaldavPortInput, text: $caldavPortText)
                        // The port the field shows is the read's: typing over
                        // an unread value would save a guess.
                        .faunaPossible(vm.toggleAnswer)
                }
                FaunaPossibilityReason(vm.toggleAnswer)
                automationText(Ids.adminCalendarCaldavPortHelpText, L.admin.calendarPage.caldavPortDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack {
                    // The draft's own reason (the read's stands under the field).
                    FaunaPossibilityReason(caldavPortDraftPossibility(draft: caldavPortText))
                    Spacer()
                    Button(L.admin.calendarPage.caldavPortSave) { saveCaldavPort() }
                        .disabled(vm.isBusy)
                        .accessibilityIdentifier(Ids.adminCalendarCaldavPortSaveButton)
                        .automationActivate(Ids.adminCalendarCaldavPortSaveButton,
                                            isEnabled: { !vm.isBusy }) {
                            saveCaldavPort()
                        }
                        // The read gate, then the typed port; `saveCaldavPort`
                        // asks the same answer.
                        .faunaPossible(vm.portSaveAnswer(caldavPortText))
                        .faunaGate("fauna.bridges.set_caldav_port")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
    }

    /// The press asks the answer the Save is painted from: an out-of-range /
    /// unparseable port is refused on `error-message` with the shared reason and
    /// NOT dispatched.
    private func saveCaldavPort() {
        if refusePress(vm.portSaveAnswer(caldavPortText), errorMessage: &vm.errorMessage) { return }
        // Shared `fauna_core::format::parse_port` (UniFFI `parsePort`) — the
        // parser the answer above asks, so this never fails once it passed.
        guard let port = parsePort(input: caldavPortText) else { return }
        Task { await vm.setCaldavPort(port) }
    }

    // MARK: - Deployment-wide toggle (dispatch-on-change)

    private func enableGroup(_ snap: CaldavPolicySnapshot) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Toggle(L.admin.calendarPage.enabledLabel, isOn: Binding(
                    get: { snap.caldavEnabled },
                    set: { on in Task { await vm.setCaldavEnabled(on) } }
                ))
                .accessibilityIdentifier(Ids.adminCalendarEnabledToggle)
                // Read the LIVE snapshot (not the captured `snap`) and flip via the
                // SAME VM dispatch the Toggle's setter runs.
                .automationActivate(
                    Ids.adminCalendarEnabledToggle,
                    value: { (vm.snapshot?.caldavEnabled ?? false) ? "on" : "off" }
                ) {
                    let next = !(vm.snapshot?.caldavEnabled ?? false)
                    Task { await vm.setCaldavEnabled(next) }
                }
                // Waits for `get_mail_config` (admin.md § Errors & edge cases,
                // *Controls wait for their read*) — never flipped from the
                // `false` a machine starts at.
                .faunaPossible(vm.toggleAnswer)
                .faunaGate("fauna.bridges.set_caldav_enabled")
                Text(L.admin.calendarPage.enabledSubtitle)
                    .font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
    }
}
#endif // !FAUNA_KIDS

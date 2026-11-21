// Excised from the Fauna Kids build with the rest of the mail plane
// (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The apple apps' one holder of the shared mail-usable answer
/// (`docs/goal/ui/mail-settings.md` § Errors & edge cases, the mail-usable
/// gate): whether the six mail sub-pages — Mail aliases, Mail spam, Export
/// mailbox, Import mailbox, Mail lists, Mail list members — and their Settings
/// rail rows can be offered now.
///
/// The answer is decided in shared Rust, `MailSettingsSnapshot::mail_usable()`
/// over UniFFI's `mailSettingsMailUsable(snapshot:)`; this type never derives
/// it from `enabled` or a domain list (`ui/README.md` § Offer only what is
/// possible, rule 6). Its three sources are the three shared exports:
/// `mailUsableUnread()` before a machine exists, `mailUsableUnbuilt()` when the
/// build failed (rule 7 — never silent controls), and the machine's own
/// snapshot once it has one. tui's twin is `SettingsState::mail_usable`
/// (`apps/fauna-tui/src/settings/mod.rs`).
///
/// One per ``FaunaClient`` (one per signed-in session), so a session change
/// starts unread. The rail reads it before a mail row is reached — each rail's
/// `.task` calls ``refresh(api:)`` — and the Mail & Calendar page feeds it its
/// own snapshot after every hydrate and dispatch (``observe(_:)``), so turning
/// mail on lifts the gate without leaving the page.
@MainActor @Observable
public final class MailUsableGate {
    /// The current shared answer for every gated page.
    public private(set) var answer: Possibility = mailUsableUnread()

    /// The six pages the gate covers — exactly the rows tui gates
    /// (`settings/root.rs`, the `.possible(mail_usable…)` rows). Mail & Calendar
    /// itself is NOT among them: it is the one step that unblocks the rest and
    /// stays live (rule 3); Import from other services is not a mail page.
    public static let gatedPages: Set<SettingsPage> = [
        .mailAliases, .mailSpam, .mailExport, .mailImport, .mailLists, .mailListMembers,
    ]

    private var machine: MailSettingsMachine?
    private var machineApi: APIClient?
    private var refreshing = false

    public init() {}

    /// The answer one Settings page is painted from: the gate's for the six
    /// mail sub-pages, possible for every other page.
    public func answer(for page: SettingsPage) -> Possibility {
        Self.gatedPages.contains(page) ? answer : .possible
    }

    /// Paint from a snapshot the caller already holds (the Mail & Calendar
    /// page's machine after its hydrate or dispatch).
    public func observe(_ snapshot: MailSettingsSnapshot) {
        answer = mailSettingsMailUsable(snapshot: snapshot)
    }

    /// The machine could not be built on this device.
    public func observeUnbuilt() {
        answer = mailUsableUnbuilt()
    }

    /// Read the nest: build this gate's machine once per `APIClient`, hydrate
    /// it, paint its snapshot. A failed build answers unbuilt; a failed hydrate
    /// leaves the snapshot unread (its `read` stays false), so the answer stays
    /// blocked-waiting rather than guessing (rule 5). Concurrent calls collapse
    /// to the one in flight.
    public func refresh(api: APIClient) async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        if machine == nil || machineApi !== api {
            do {
                machine = try await api.mailSettingsMachine()
                machineApi = api
            } catch {
                machine = nil
                machineApi = nil
                observeUnbuilt()
                return
            }
        }
        guard let machine else { return }
        try? await machine.hydrate()
        observe(machine.snapshot())
    }
}

/// What a gated mail sub-page shows in place of its content while the answer
/// is not possible: the reason the shared answer gives, or — on a box that can
/// never serve mail, reached by a door other than the (absent) rail row — the
/// Mail & Calendar page's own unavailable notice, the fact the person reads
/// there too. `nil` when possible (the page's own content shows).
public func mailSubpageGateText(_ answer: Possibility) -> String? {
    switch answer {
    case .possible: return nil
    case .blocked: return answer.reasonText ?? ""
    case .absent: return L.mailSettings.unavailableNotice
    }
}

/// The gate at a mail sub-page's ENTRY (`ui/README.md` § Offer only what is
/// possible, rule 4): while the shared answer is not possible the page opens
/// on its heading and the reason only — none of its forms, sheets or wizards,
/// whose submit could not succeed. Back is the shell's own (the navigation
/// stack's on iOS, `settings-nav-back` in the macOS rail). The rail row is
/// gated on the same answer; this covers every other door in (the automation
/// driver's nav, a page left open while mail was turned off). tui's twin is
/// `gate_mail_subpage` (`apps/fauna-tui/src/settings/mod.rs`).
public struct MailSubpageGate<Content: View>: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    private let page: SettingsPage
    private let content: () -> Content

    public init(_ page: SettingsPage, @ViewBuilder content: @escaping () -> Content) {
        self.page = page
        self.content = content
    }

    public var body: some View {
        let answer = client?.mailUsable.answer(for: page) ?? mailUsableUnread()
        Group {
            if let text = mailSubpageGateText(answer) {
                VStack(alignment: .leading, spacing: 12) {
                    automationText("mail-subpage-gate-reason", text)
                        .foregroundStyle(.secondary)
                }
                .padding()
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .pageTitle(page.label)
            } else {
                content()
            }
        }
        .task {
            guard let client else { return }
            await client.mailUsable.refresh(api: client.api)
        }
    }
}
#endif

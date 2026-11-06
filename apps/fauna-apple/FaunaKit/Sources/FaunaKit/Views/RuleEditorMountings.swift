import SwiftUI

/// What `rule-editor-status` says, before it is resolved to a string.
private enum HostStatus {
    case none
    case saved
    case savedVersion(UInt64)
    /// The nest's or the read's own words (a refusal names its defect).
    case text(String)

    var text: String {
        switch self {
        case .none: ""
        case .saved: L.ruleEditor.saved
        case .savedVersion(let sequence): L.ruleEditor.savedVersion(sequence: String(sequence))
        case .text(let text): text
        }
    }
}

/// The shared rule editor's two app mountings on apple (`nest-rules.md` § The
/// shared rule editor → *Three mountings, one shape*): the admin's in
/// `admin-nest-rules-section` and the guardian's in
/// `family-policy-rules-section`. Each owns its editor handle, its save and its
/// status line; the editor itself is the one ``RuleEditorPanel`` paint. tui's
/// `rule_editor.rs` mountings, web's `admin/nest` and `family` pages and
/// android's `RuleEditorMountings` are the siblings.
///
/// `admin-nest-rules-section` — the admin's mounting on `admin-nest`, directly
/// after the reports queue (`admin.md` § N. Nest → *Nest rules*): read on entry
/// over `fauna.admin.content_policy.get` beside the self-description's domain,
/// written through `.set`; the status is the reply's version or the nest's named
/// defect.
public struct AdminNestRulesSection: View {
    let api: APIClient
    let nestRules: NestRulesStore

    @State private var editor: FfiRuleEditor?
    @State private var status: HostStatus = .none
    @State private var saving = false

    public init(api: APIClient, nestRules: NestRulesStore) {
        self.api = api
        self.nestRules = nestRules
    }

    public var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.ruleEditor.nestSection).font(.headline)
                if let editor {
                    RuleEditorPanel(
                        editor: editor,
                        write: .adminContentPolicySet,
                        status: status.text,
                        saving: saving,
                        onSave: { Task { await save(editor) } },
                        onChange: { status = .none },
                        loadLabelers: { try await api.ruleLabelers() },
                        loadModule: { try await api.ruleModule(labelerIdHex: $0) })
                } else {
                    Text(status.text.isEmpty ? L.ruleEditor.loading : status.text)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminNestRulesSection)
        .automationValue(Ids.adminNestRulesSection, text: { L.ruleEditor.nestSection })
        .task { await load() }
    }

    @MainActor
    private func load() async {
        do {
            let policy = try await api.adminContentPolicy()
            guard let domain = await nestRules.domain(api: api) else {
                status = .text("the nest's domain is not known yet")
                return
            }
            editor = try FfiRuleEditor.forNest(domain: domain, document: policy.document)
        } catch {
            status = .text(DisplayError.message(error) ?? "error")
        }
    }

    @MainActor
    private func save(_ editor: FfiRuleEditor) async {
        saving = true
        defer { saving = false }
        do {
            let reply = try await api.adminSetContentPolicy(document: try editor.documentBytes())
            status = reply.defect.map { .text($0) } ?? .savedVersion(reply.sequence)
        } catch {
            status = .text(DisplayError.message(error) ?? "error")
        }
    }
}

/// `family-policy-rules-section` — the guardian's mounting in the per-ward policy
/// editor (`family-safety.md` § App surface → *Content rules*): loaded from the selected ward's policy
/// (`FfiRuleEditor.forGuardian`) and saved by its own button through the shared
/// `FfiFamilyClient.saveContentRules`; `onSaved` re-reads the page, so what
/// paints after a save is what the nest stored.
public struct FamilyRulesSection: View {
    let api: APIClient
    let ward: FfiFamilyWardInfo
    let onSaved: () async -> Void

    @State private var status: HostStatus = .none
    @State private var saving = false

    public init(api: APIClient, ward: FfiFamilyWardInfo, onSaved: @escaping () async -> Void) {
        self.api = api
        self.ward = ward
        self.onSaved = onSaved
    }

    /// A fresh editor per ward and per stored document: a re-read that changed
    /// nothing keeps the one being edited.
    private var identity: String {
        "\(data_to_hex(ward.actorId)):\(ward.policy.contentPolicy?.document.map { data_to_hex($0) } ?? "")"
    }

    public var body: some View {
        FamilyRulesEditorHost(
            api: api, ward: ward, status: $status, saving: $saving, onSaved: onSaved
        )
        .id(identity)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.familyPolicyRulesSection)
        .automationValue(Ids.familyPolicyRulesSection, text: { L.ruleEditor.familySection })
    }
}

/// The guardian mounting's body, keyed by ``FamilyRulesSection/identity`` so its
/// editor handle is rebuilt only when the ward or its stored document changes.
private struct FamilyRulesEditorHost: View {
    let api: APIClient
    let ward: FfiFamilyWardInfo
    @Binding var status: HostStatus
    @Binding var saving: Bool
    let onSaved: () async -> Void

    @State private var editor: FfiRuleEditor

    init(
        api: APIClient, ward: FfiFamilyWardInfo, status: Binding<HostStatus>,
        saving: Binding<Bool>, onSaved: @escaping () async -> Void
    ) {
        self.api = api
        self.ward = ward
        _status = status
        _saving = saving
        self.onSaved = onSaved
        _editor = State(initialValue: FfiRuleEditor.forGuardian(policy: ward.policy.contentPolicy))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.ruleEditor.familySection).font(.subheadline.weight(.semibold))
            RuleEditorPanel(
                editor: editor,
                write: .familyPolicyUpdate,
                status: status.text,
                saving: saving,
                onSave: { Task { await save() } },
                onChange: { status = .none },
                loadLabelers: { try await api.ruleLabelers() },
                loadModule: { try await api.ruleModule(labelerIdHex: $0) })
        }
    }

    @MainActor
    private func save() async {
        saving = true
        defer { saving = false }
        do {
            // `nil`: the caller no longer guards the ward and nothing was
            // written — the re-read drops it from the list (web's twin).
            let entry = try await api.familySaveContentRules(
                ward: ward.actorId, document: try editor.documentBytes())
            await onSaved()
            status = entry == nil ? .none : .saved
        } catch {
            status = .text(DisplayError.message(error) ?? "error")
        }
    }
}

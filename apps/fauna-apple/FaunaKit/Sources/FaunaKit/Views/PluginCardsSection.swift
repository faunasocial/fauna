// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// State behind the plugin settings cards on the Bridges page
/// (`behavior/bridges.md` § Hosted plugins on this page). Every card is composed
/// once in shared Rust (`FfiPluginCard`); this holds the list, the failure line
/// and the connected-apps machine Connect and Disconnect go through.
@MainActor @Observable
public final class PluginCardsVM {
    /// `nil` until the first read returned.
    public private(set) var cards: [FfiPluginCard]?
    public private(set) var bindOpened = false
    public private(set) var errorMessage: String?

    private var api: APIClient?
    private var machine: ConnectedAppsMachine?

    public init() {}

    public func configure(api: APIClient) {
        if let current = self.api, current !== api { reset() }
        self.api = api
    }

    /// The host page's `.task(id:)` body: bind to the session's client and read.
    public func start(client: FaunaClient?) async {
        guard let client else {
            reset()
            return
        }
        configure(api: client.api)
        await refresh()
    }

    public func reset() {
        cards = nil
        bindOpened = false
        errorMessage = nil
        machine = nil
        api = nil
    }

    public func refresh() async {
        guard let api else { return }
        do {
            cards = try await api.listPluginCards()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// *Connect* while the card is unbound (`bindPlugin` on the install row),
    /// *Disconnect* while bound (`revokePrincipal` on this account's binding
    /// row); the machine's page error, when it set one, is the failure.
    public func press(_ card: FfiPluginCard) async {
        errorMessage = nil
        bindOpened = false
        guard let api else { return }
        do {
            let built: ConnectedAppsMachine
            if let machine {
                built = machine
            } else {
                built = try await api.connectedAppsMachineWithoutPush()
                machine = built
            }
            if card.bound, let binding = card.bindingPrincipalIdHex {
                await built.revokePrincipal(principalId: hex_to_data(binding))
            } else {
                await built.bindPlugin(principalId: hex_to_data(card.principalIdHex))
            }
            if let failed = built.snapshot().error.map(renderLocalizedText) {
                errorMessage = failed
                return
            }
            if !card.bound { bindOpened = true }
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// `fauna.plugins.set_settings` for one setting of a bound plugin, then re-read.
    public func update(_ card: FfiPluginCard, key: String, value: FfiCborValue) async {
        guard let api else { return }
        do {
            try await api.setPluginSetting(principalIdHex: card.principalIdHex, key: key, value: value)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
        await refresh()
    }
}

/// Every account's plugin settings cards on the Bridges page — one more
/// `bridge-card` after the bridges, painted from the shared `plugin_card`
/// composition. Reference painter: tui
/// `apps/fauna-tui/src/bridges.rs::plugin_card_elements`.
public struct PluginCardsSection: View {
    private let vm: PluginCardsVM
    /// The display index of the first plugin card: the bridges come first.
    private let startIndex: Int

    public init(vm: PluginCardsVM, startIndex: Int) {
        self.vm = vm
        self.startIndex = startIndex
    }

    /// Sections, so a host `List`/`Form` lays one per card and paints nothing
    /// at all while there are no cards.
    public var body: some View {
        ForEach(Array((vm.cards ?? []).enumerated()), id: \.offset) { offset, card in
            Section {
                cardView(card, index: startIndex + offset)
            }
        }
        if vm.bindOpened || vm.errorMessage != nil {
            Section {
                if vm.bindOpened {
                    Text(L.bridges.pluginBindOpened)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
        }
    }

    private func cardView(_ card: FfiPluginCard, index: Int) -> some View {
        let badge = renderLocalizedText(card.badge)
        let publisher = renderLocalizedText(card.publisher)
        return VStack(alignment: .leading, spacing: 8) {
            // `bridge-card-name` — the card's title, inside its `bridge-card[i]`
            // scope like a bridge card's.
            automationText(Ids.bridgeCardName, card.name).font(.headline)
            Text("\(badge)\n\(publisher)").font(.caption).foregroundStyle(.secondary)
            Button(
                card.bound ? L.bridges.pluginDisconnect : L.bridges.pluginConnect,
                role: card.bound ? .destructive : nil
            ) {
                Task { await vm.press(card) }
            }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier(Ids.bridgeActionButton)
            .automationActivate(
                Ids.bridgeActionButton,
                value: { card.bound ? "linked" : "unlinked" },
                attributes: { ["client_id": card.clientId, "bound": card.bound ? "true" : "false"] }
            ) {
                Task { await vm.press(card) }
            }
            ForEach(card.settings, id: \.key) { setting in
                settingRow(card, setting)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.bridgeCard)
        .automationValue(Ids.bridgeCard, text: { card.name })
        .automationScope(Ids.bridgeCard, index: index)
    }

    /// Which control paints is the shared classifier's call
    /// (`fauna_client_bridges::setting_control`) — no spelling is matched here.
    @ViewBuilder
    private func settingRow(_ card: FfiPluginCard, _ setting: FfiBridgeSetting) -> some View {
        let id = "\(Ids.bridgeSetting)[\(setting.key)]"
        switch settingControl(settingType: setting.settingType) {
        case .toggle:
            let checked = setting.value.pluginBool ?? false
            Toggle(setting.label, isOn: Binding(
                get: { checked },
                set: { v in Task { await vm.update(card, key: setting.key, value: .bool(v: v)) } }
            ))
            .accessibilityIdentifier(id)
            .automationActivate(
                id,
                value: { checked ? "on" : "off" },
                attributes: { ["checked": checked ? "true" : "false"] }
            ) {
                Task { await vm.update(card, key: setting.key, value: .bool(v: !checked)) }
            }
        case .number:
            PluginSettingTextRow(
                id: id, label: setting.label, current: setting.value.pluginInt.map(String.init) ?? "",
                number: true
            ) { text in
                guard let parsed = parseCountI64(input: text) else { return }
                Task { await vm.update(card, key: setting.key, value: .integer(v: parsed)) }
            }
        case .text:
            PluginSettingTextRow(
                id: id, label: setting.label, current: setting.value.pluginText ?? "", number: false
            ) { text in
                Task { await vm.update(card, key: setting.key, value: .text(v: text)) }
            }
        case .select:
            Picker(setting.label, selection: Binding(
                get: { setting.value.pluginText ?? "" },
                set: { v in Task { await vm.update(card, key: setting.key, value: .text(v: v)) } }
            )) {
                ForEach(Array((setting.options ?? []).enumerated()), id: \.offset) { _, option in
                    Text(option.label).tag(option.value.pluginText ?? "")
                }
            }
            .accessibilityIdentifier(id)
            .automationValue(id, text: { setting.value.pluginText }, value: { setting.value.pluginText })
        case .unknown:
            // Read-only line: a setting_type this build predates is shown, never dropped.
            LabeledContent(setting.label) {
                Text(setting.value.pluginText ?? "").font(.caption)
            }
        }
    }
}

/// A `text`/`number` plugin setting: type-then-commit on submit (the bridge
/// setting rows' shape — the change saves on the spot, no save control).
private struct PluginSettingTextRow: View {
    let id: String
    let label: String
    let current: String
    let number: Bool
    let commit: (String) -> Void
    @State private var text = ""

    var body: some View {
        LabeledContent(label) {
            TextField("", text: $text)
                #if os(iOS)
                .keyboardType(number ? .numberPad : .default)
                #endif
                .multilineTextAlignment(.trailing)
                .onSubmit { commit(text) }
                .accessibilityIdentifier(id)
                .automationField(id, text: $text, commit: { commit(text) })
        }
        .task(id: current) { text = current }
    }
}

extension FfiCborValue {
    fileprivate var pluginBool: Bool? {
        if case .bool(let v) = self { return v }
        return nil
    }
    fileprivate var pluginInt: Int64? {
        if case .integer(let v) = self { return v }
        return nil
    }
    fileprivate var pluginText: String? {
        if case .text(let v) = self { return v }
        return nil
    }
}
#endif // !FAUNA_KIDS

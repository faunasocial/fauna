import SwiftUI

/// Horizontal row of provider buttons used by both `MacDnsConfigView` and
/// `MacVpsConfigView` (macOS) and `DnsConfigView` (iOS). The shape mirrors the
/// iOS / web counterparts.
///
/// `kind` is the ui.yaml prefix — "dns" or "vps". The composite row exposes
/// `<kind>-provider-row` as a container ID and per-provider buttons as
/// `<kind>-provider-row[<provider_id>]` (matches `tests/e2e-unified/drivers/scope.py`'s
/// indexed-id syntax).
public struct ProviderRow: View {
    public let providers: [ProviderMeta]
    public let selectedId: String?
    public let kind: String
    public let isEnabled: (ProviderMeta) -> Bool
    /// Copy comprehensibility rule 5 — the reason a disabled row is disabled,
    /// shown beside it (`ui/README.md` § Copy comprehensibility). `vps_config`
    /// gates nothing per-row today (`isEnabled` is always true there), so it
    /// passes no closure and gets the default no-reason one.
    public let reasonFor: (ProviderMeta) -> LocalizedText?
    public let onSelect: (String) -> Void

    public init(
        providers: [ProviderMeta],
        selectedId: String?,
        kind: String,
        isEnabled: @escaping (ProviderMeta) -> Bool,
        reasonFor: @escaping (ProviderMeta) -> LocalizedText? = { _ in nil },
        onSelect: @escaping (String) -> Void
    ) {
        self.providers = providers
        self.selectedId = selectedId
        self.kind = kind
        self.isEnabled = isEnabled
        self.reasonFor = reasonFor
        self.onSelect = onSelect
    }

    public var body: some View {
        HStack(alignment: .top) {
            ForEach(providers, id: \.id) { p in
                VStack(alignment: .leading, spacing: 2) {
                    Button(L.lookup(p.displayNameKey)) { onSelect(p.id) }
                        .buttonStyle(.bordered)
                        .tint(selectedId == p.id ? .accentColor : .secondary)
                        .disabled(!isEnabled(p))
                        .accessibilityIdentifier("\(kind)-provider-row[\(p.id)]")
                        .automationActivate(
                            "\(kind)-provider-row[\(p.id)]",
                            isEnabled: { isEnabled(p) }
                        ) { onSelect(p.id) }
                    if !isEnabled(p), let reason = reasonFor(p) {
                        Text(renderLocalizedText(reason))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        // Surface the row itself as an accessibility container so XCUITest
        // (and the apple-bridge) can find `<kind>-provider-row` and its
        // indexed children. Without `.contain`, SwiftUI's default behaviour
        // for HStack would push the identifier onto a single combined node
        // and hide the per-provider buttons.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("\(kind)-provider-row")
        // The bare `.accessibilityIdentifier` above is INVISIBLE to the in-process
        // driver's `AutomationRegistry` (it populates only the a11y/XCUITest tree), so
        // `count("<kind>-provider-row")` / `wait_for(...)` read 0 even though the indexed
        // children register via `.automationActivate`. Register the container itself as a
        // countable presence anchor (text "" — presence only), the documented countable-
        // container pattern (device-card / admin-stat-card / snapshot-item). web (+page.svelte
        // container div) + linux (vps_config.rs GtkBox) count the bare container the same way.
        .automationValue("\(kind)-provider-row", text: { "" })
    }
}

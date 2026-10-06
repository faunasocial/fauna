import SwiftUI
import FaunaKit

struct MacStatusView: View {
    @Environment(MacAppState.self) private var appState
    // The quota fetch needs the client's APIClient — `StatusVM.fetchQuota()`
    // early-returns until `configure(api:)` runs (mirrors the iOS
    // `SettingsView`/`StatusDetailView` wiring). Every path that builds a client
    // writes both the App's `@State client` and the observed
    // `appState.liveClient`; the injection asks the observed one FIRST
    // (FaunaMacApp injects `appState.liveClient ?? client`), because a `@State`
    // write from a callback can leave `client` holding a replaced object.
    @Environment(FaunaClient.self) private var client: FaunaClient?
    // The MLS leg's channel count is the conversations manager's own
    // (`secureChannelCount()`); the Sync leg is the local agent's backlog off
    // `SyncAgentHealthModel`'s 10 s tick (`ui/status.md` § State & data shape).
    @Environment(ConversationsVM.self) private var conversationsVM
    @Environment(SyncAgentHealthModel.self) private var syncAgentHealth: SyncAgentHealthModel?
    @State private var vm = StatusVM()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                // Identity
                GroupBox(L.common.identity) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let actorId = appState.session.actorId {
                            LabeledContent(L.common.actorId) {
                                HStack(spacing: 4) {
                                    // Live-data placement: the Status sub-page carries
                                    // `account-actor-id` (settings.md:20) so a plain
                                    // Settings `navigate()` (lands here) keeps
                                    // `test_actor_id_visible` green. Leaf id only — a
                                    // container id would clobber the copy button's.
                                    automationText(Ids.accountActorId, actorId)
                                        .textSelection(.enabled)
                                        .lineLimit(1)
                                        .truncationMode(.middle)
                                    CopyButton(Ids.statusActorIdCopyBtn, text: actorId)
                                }
                            }
                        }
                        if let nodeUrl = appState.session.nodeUrl {
                            LabeledContent("\(L.status.node.title):") {
                                HStack(spacing: 4) {
                                    Text(nodeUrl)
                                        .textSelection(.enabled)
                                    CopyButton(Ids.statusNodeUrlCopyBtn, text: nodeUrl)
                                }
                            }
                        }
                    }
                    .padding(8)
                    // Page landmark (ui/settings.md § Element IDs — "account-settings-link"
                    // as page landmark; test_navigation.py's cross-app smoke test). `.contain`
                    // keeps this container id AND the child ids (account-actor-id, …) queryable
                    // — the container-clobber rule, mirrors quota-section below. Needs its own
                    // `.automationValue` too, not just `.accessibilityIdentifier` — the
                    // in-process `AutomationRegistry` only registers presence off
                    // `.automationValue`/`.automationActivate`, so an id-only container is
                    // invisible to `is_visible()`/`wait_for()` even though its children register
                    // fine (found + fixed on `critical-alerts`, same session).
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.accountSettingsLink)
                    .automationValue(Ids.accountSettingsLink, text: { "" })
                }

                // Quota — live-data placement on the Status sub-page (settings.md:20)
                // so a plain Settings `navigate()` keeps `test_quota_section` green.
                // `.accessibilityElement(children: .contain)` keeps both the
                // `quota-section` container id AND the per-row child ids queryable
                // (the container-clobber rule — see the per-card pattern).
                if let quota = vm.quota {
                    GroupBox(L.status.quota.title) {
                        VStack(alignment: .leading, spacing: 8) {
                            LabeledContent(L.common.tier) { Text(quota.tier) }
                            QuotaBar(label: L.common.inbox,
                                     used: quota.inbox.usedBytes,
                                     max: quota.inbox.maxBytes)
                                .accessibilityIdentifier(Ids.quotaInbox)
                                .automationValue(Ids.quotaInbox,
                                                 text: { "\(quota.inbox.usedBytes) / \(quota.inbox.maxBytes)" })
                            QuotaBar(label: L.common.storage,
                                     used: quota.storage.usedBytes,
                                     max: quota.storage.maxBytes)
                                .accessibilityIdentifier(Ids.quotaStorage)
                                .automationValue(Ids.quotaStorage,
                                                 text: { "\(quota.storage.usedBytes) / \(quota.storage.maxBytes)" })
                            LabeledContent(L.common.devices) {
                                Text("\(quota.devices.used) / \(quota.devices.max)")
                            }
                            .accessibilityIdentifier(Ids.quotaDevices)
                            .automationValue(Ids.quotaDevices,
                                             text: { "\(quota.devices.used) / \(quota.devices.max)" })
                        }
                        .padding(8)
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.quotaSection)
                    .automationValue(Ids.quotaSection, text: { quota.tier })
                }

                // Feature limits — placed directly after Quota as its sibling
                // "what bounds me" surface (settings.md § Layout & flow item
                // 2b, dynamic-features.md § Transparency & auditability).
                // `.contain` mirrors the quota-section container-clobber rule.
                if let featureRows = vm.featureRows {
                    GroupBox(L.features.sectionTitle) {
                        FeatureLimitsSection(rows: featureRows)
                            .padding(8)
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.featureLimitsSection)
                    .automationValue(Ids.featureLimitsSection, text: { L.features.sectionTitle })
                }

                // Region — the region content plane's transparency surface
                // (region-blocking.md § The blocked render and the transparency
                // surface), after feature limits as on linux and web.
                GroupBox(L.region.sectionTitle) {
                    RegionSettingsSection(region: appState.region)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.settingsRegionSection)
                .automationValue(Ids.settingsRegionSection, text: { L.region.sectionTitle })

                // Node, Sync, Encryption and Build — the shared snapshot's
                // legs (`ui/status.md` § Layout & flow sections 5–8), painted
                // from `fauna_client_status::render`'s texts; a section whose
                // leg is not loaded is not painted.
                StatusLegSections(text: vm.sectionText())

                // Actions
                GroupBox(L.common.actions) {
                    VStack(alignment: .leading, spacing: 8) {
                        Button(L.status.actions.clearCache) {
                            vm.clearCache()
                        }
                        // Sign-out now lives on the Account settings page (uniform
                        // inline confirm — see SignOutSection / settings.md § User actions).
                    }
                    .padding(8)
                }
            }
            .padding()
        }
        // `settings-view` — the settings-shell "loaded" landmark the cross-app
        // `test_settings_page_loads` reads. Status is the default `{"view":"settings"}`
        // landing, so it carries the marker (mirrors linux `views/status.rs:42`).
        // `.contain` keeps the child ids (account-actor-id, quota-section, …) queryable.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.settingsView)
        .automationValue(Ids.settingsView, text: { "" })
        .navigationTitle(L.common.status)
        // The drop below is REDUNDANT on macOS and carried for uniformity, as
        // `SearchVM`'s is: `tearDownSessionForSwitch()` sets `isOnboarded = false`,
        // which unmounts `MainWindowView` wholesale, so this view dies with the window
        // and its view model with it. That unmount IS the guarantee here
        // (`account-scoping.md` § The scoping taxonomy, the in-memory corollary: an app
        // whose drop rides a shell teardown must say where the guarantee comes from) —
        // which is exactly what iOS does not have, and why the seam lives on the view
        // model rather than at either site .
        .task(id: SessionKey(client)) {
            // Wire the VM to the live (or test-agent) client before fetching, else
            // `fetchQuota()` no-ops and `quota-section` never renders — the
            // regression the shell lift introduced (test_quota_section).
            guard let client else {
                vm.reset()
                return
            }
            vm.configure(api: client.api)
            await vm.refreshQuotaAndLimits()
            await loadStatusLegs()
        }
        // The sync leg follows the agent's poll: each 10 s tick replaces it (or
        // clears it when the agent stops answering).
        .onChange(of: syncAgentHealth?.syncLeg, initial: true) { _, leg in
            vm.setSyncLeg(leg)
        }
        // Re-pull account quota + feature limits on WS-RPC reconnect (mirrors linux).
        .onReconnect {
            await vm.refreshQuotaAndLimits()
            await loadStatusLegs()
        }
    }

    /// The node and MLS legs — one nest read each per visit / reconnect.
    private func loadStatusLegs() async {
        let manager = conversationsVM.manager
        await vm.fetchStatusLegs(
            actorId: appState.session.actorId,
            secureChannelCount: { manager.secureChannelCount() })
    }
}

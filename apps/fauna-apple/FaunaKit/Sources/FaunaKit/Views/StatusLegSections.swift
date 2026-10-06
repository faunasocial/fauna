import SwiftUI

/// The Status surface's Node, Sync, Encryption and Build sections
/// (`docs/goal/ui/status.md` § Layout & flow sections 5–8), painted from the
/// shared snapshot's text projection (`fauna_client_status::render`, over the
/// FFI as `statusText`). Shared FaunaKit content consumed by macOS
/// (`MacStatusView`) and iOS (`StatusDetailView`) so the two cannot diverge —
/// the same lift as ``FeatureLimitsSection``.
///
/// **A section whose leg is `nil` is not painted at all** — no header, no
/// placeholder, no zero (the un-hydrated-paint rule; it is also what lets a
/// witness wait for the real value). The element text is the bare value every
/// app's witness reads; the human label beside it is the shared `status.*` /
/// `common.*` string and never part of the element text. iOS has no local sync
/// agent, so its Sync leg is always `nil` and the Sync section never appears
/// there (`status.md` § State & data shape, the sync leg).
public struct StatusLegSections: View {
    let text: FfiStatusText

    public init(text: FfiStatusText) {
        self.text = text
    }

    public var body: some View {
        if let domain = text.nodeDomain, let version = text.nodeVersion {
            section(L.status.node.title) {
                row(L.common.domain, Ids.statusNodeDomain, domain)
                row(L.admin.dashboard.version, Ids.statusNodeVersion, version)
            }
        }
        if let pending = text.syncPending, let last = text.syncLast {
            section(L.common.sync) {
                row(L.common.pending, Ids.statusSyncPending, pending)
                row(L.status.sync.lastSync, Ids.statusSyncLast, last)
            }
        }
        if let keyPackages = text.mlsKeyPackages, let channels = text.mlsChannels {
            section(L.settings.encryptionPage.title) {
                row(L.status.encryption.keyPackages, Ids.statusMlsKeyPackages, keyPackages)
                row(L.status.encryption.dmChannels, Ids.statusMlsChannels, channels)
            }
        }
        // Build: absent on an unstamped build — never a `dev` placeholder.
        if let sha = text.buildSha {
            section(L.status.build.title) {
                row(L.status.build.commit, Ids.statusBuildSha, sha)
            }
        }
    }

    private func section<Content: View>(
        _ title: String, @ViewBuilder content: () -> Content
    ) -> some View {
        GroupBox(title) {
            VStack(alignment: .leading, spacing: 8) {
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
        }
    }

    private func row(_ label: String, _ id: String, _ value: String) -> some View {
        LabeledContent(label) {
            automationText(id, value)
                .textSelection(.enabled)
        }
    }
}

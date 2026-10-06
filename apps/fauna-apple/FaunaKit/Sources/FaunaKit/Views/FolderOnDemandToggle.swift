import FaunaFFISwift
import SwiftUI

#if canImport(FileProvider)

    /// Per-set, per-device on-demand presence (`folder-on-demand-toggle`, indexed;
    /// macOS "Show in Finder" / iOS "Show in Files") — on EVERY owner row, never
    /// gated on `folder.mode` (`ui/folders.md`), and on every row of a folder
    /// shared WITH the account (`hideOnly`), **default ON** (`on-demand-files.md`
    /// § Apple File Provider binding, *Auto-appear default-ON*). Backed by the
    /// device-local `FileProviderDomainPrefs` store (sanctioned device-local
    /// config — never nest state) and converged by the caller's `onChanged` hook
    /// (the app's `FileProviderCoordinator.reconcile` closure), so a flip
    /// adds/removes the set's Finder/Files domain immediately.
    ///
    /// **Turning it ON is the enrol gesture:** on a row where this device holds
    /// no place it writes this device's place at the default point through the
    /// shared `ensure_place` (`file-sync.md` § 4, *A local presence writes the
    /// place it needs*) — BEFORE `onChanged` re-plans the domains, so the plan
    /// sees the place the presence needs. A device that already holds a place
    /// keeps it untouched. **On a member's row it is hide-only** (`hideOnly`):
    /// the membership is the seat, so no place is read or written — the flip
    /// only persists the preference and re-plans (`on-demand-files.md` § Shared
    /// sets on a capability host, decision 3).
    ///
    /// **Arbitration surface:** a set with a bound always-resident folder shows the
    /// binding as its one local presence instead of this toggle (bound folder > FP
    /// domain; the toggle re-decides only after an unbind) — the caption rendered
    /// in the bound case says exactly that. iOS has no binding surface, so there
    /// `isBound` is always false.
    struct FolderOnDemandToggle: View {
        @Environment(FaunaClient.self) private var client: FaunaClient?
        let vm: DevicesMachineVM
        let folder: FolderSummary
        let isBound: Bool
        /// Re-converge the FP domains after a flip; `nil` (no host app hook, e.g.
        /// previews) just persists the preference — the next launch reconcile
        /// applies it.
        let onChanged: (() -> Void)?
        /// A row of a folder shared with the account: the flip never enrols.
        var hideOnly: Bool = false

        @State private var isOn: Bool = true

        private var title: String {
            #if os(macOS)
                L.devices.showOnDemandFinder
            #else
                L.devices.showOnDemandFiles
            #endif
        }

        var body: some View {
            if isBound {
                // The binding IS the local presence — no toggle to offer.
                Text(L.devices.onDemandBoundHint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 2) {
                    Toggle(isOn: Binding(get: { isOn }, set: { apply($0) })) {
                        Text(title)
                    }
                    .accessibilityIdentifier(Ids.folderOnDemandToggle)
                    // One Entry carries the click AND the "on"/"off" read (the
                    // folder-webdav-toggle template). Env-gated no-op in production.
                    .automationActivate(
                        Ids.folderOnDemandToggle,
                        value: { isOn ? "on" : "off" }
                    ) { apply(!isOn) }

                    Text(L.devices.showOnDemandHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .onAppear {
                    isOn = identity.map { FileProviderDomainPrefs.isEnabled(domainId: $0.domainId) }
                        ?? true
                }
            }
        }

        /// The preference is keyed by the set's actor-scoped identity — the
        /// domain identifier the reconcile reads it under, scoped to the
        /// session's account exactly as the reconcile scopes it (per set per
        /// ACCOUNT per device, so another account's same-numbered ref on this
        /// device keeps its own toggle). A row resolves the same ref the
        /// presence plan keys the set by (a member's row too, a cross-nest
        /// one included). An owner row always resolves a ref
        /// and a session always has an actor; a row that somehow doesn't has
        /// no domain to steer, so the flip persists nothing.
        private var identity: FileProviderDomainIdentity? {
            guard let actorHex = FaunaClient.activeActorIdHex, let folderId = folder.folderRef
            else { return nil }
            return FileProviderDomainIdentity(actorIdHex: actorHex, folderId: folderId)
        }

        private func apply(_ on: Bool) {
            if let identity {
                FileProviderDomainPrefs.setEnabled(on, domainId: identity.domainId)
            }
            isOn = on
            guard on, !hideOnly, let deviceId = client?.deviceId, !deviceId.isEmpty else {
                onChanged?()
                return
            }
            // Enrol first, re-plan second: the reconcile must see the place.
            let name = folder.name
            Task {
                await vm.ensureFolderPlace(name: name, deviceId: deviceId)
                onChanged?()
            }
        }
    }

#endif

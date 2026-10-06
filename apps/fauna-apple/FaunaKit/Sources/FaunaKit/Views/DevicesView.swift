import SwiftUI

/// Shared (macOS + iOS) **Settings → Devices** page shell. A thin wrapper around
/// `DevicesContent` (the device **roster only**) driven by the shared-Rust
/// `DevicesMachine` — the same renderer both apple targets use, so this one view
/// replaces what were two byte-identical per-platform shells (`DevicesView` on
/// iOS, `MacDevicesView` on macOS).
///
/// The `DevicesMachineVM` is the session's one instance, injected by the app
/// shell (`.environment(devicesVM)`) — a page-local VM rebuilt the machine per
/// visit and lost its memory (`ui/folders.md` § Implementation status today).
///
/// `reloadToken` is each platform's own `appState.navGeneration` (bumped on every
/// nav patch); keying `.task` on it re-`configure()`s (builds the machine once,
/// then `refresh()`es) when the page becomes visible — including a
/// devices→devices re-navigation that otherwise wouldn't re-fire (macOS's
/// `test_device_cards` reload contract, `docs/goal/ui/devices.md` § Layout &
/// flow / § Errors).
public struct DevicesView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The session's one VM (app-scene-level, shared with Settings → Folders).
    @Environment(DevicesMachineVM.self) private var vm
    let reloadToken: Int

    public init(reloadToken: Int) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        // The badge's row is the shared rule's answer (enrolled row, else this
        // app's own id), read on every appear; until the first read lands the
        // rule's own fallback — the app's id — stands in.
        DevicesContent(vm: vm, localDeviceId: vm.thisDeviceRow ?? client?.deviceId,
                       actorId: FaunaClient.activeActorIdHex)
            .pageTitle(L.devices.title)
            .task(id: reloadToken) {
                if let client {
                    await vm.configure(api: client.api)
                    await vm.loadThisDeviceRow(ownDeviceId: client.deviceId)
                }
            }
    }
}

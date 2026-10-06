import SwiftUI

/// One `device-folder-role-badge` chip: a device's place in one folder, stated
/// by its three flags (`originates` / `accepts` / `appliesDeletes`) — the only
/// shape a place has (`docs/goal/behavior/folders.md`). The label is composed
/// by the shared `devicePlaceLabel` (`fauna_core::format::device_place_label`),
/// whose two- and three-flag templates carry i18n KEYS as argument values, so
/// it renders through `renderLocalizedTextNested` — a plain resolve would paint
/// the raw `devices.wizard.place_*` keys. No flag point outranks another, so
/// every chip wears the one neutral style.
public struct DeviceFolderRoleBadge: View {
    public let originates: Bool
    public let accepts: Bool
    public let appliesDeletes: Bool

    public init(originates: Bool, accepts: Bool, appliesDeletes: Bool) {
        self.originates = originates
        self.accepts = accepts
        self.appliesDeletes = appliesDeletes
    }

    public var body: some View {
        // `automationText` renders the label AND registers its read for the
        // in-process e2e driver (a bare `.accessibilityIdentifier` is invisible
        // to it, per `apple-e2e-automation.md`'s nest-provisioning precedent) —
        // the canonical presence-readable-badge pattern also used by the sibling
        // `UnverifiedSourceBadge`.
        automationText(
            Ids.deviceFolderRoleBadge,
            renderLocalizedTextNested(devicePlaceLabel(
                originates: originates, accepts: accepts, appliesDeletes: appliesDeletes)))
            .tintedCapsuleBadge(.secondary)
    }
}

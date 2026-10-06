import Foundation

/// Which of a device place's three flags a checkbox names. The flag
/// *meanings* live once, in `fauna_protocol::folders::PlaceFlags` (Rust,
/// deliberately UniFFI-free — `ui/folders.md` § Implementation status today
/// records why three bools cross instead of the typed struct); this is the
/// flag *identity* half shared between `FolderWizardSheetView`'s and
/// `FoldersContent`'s own private `PlaceFlagBox` enums, whose `label`/`desc`
/// bodies were two byte-identical 3-case switches on the same generated i18n
/// constants — the labels are deliberately the wizard's everywhere, so the
/// same flag reads identically wherever it is edited. Each `PlaceFlagBox`
/// keeps its own `id` (a distinct automation-id namespace per screen) and its
/// own read/mutation methods (different backing FFI types), and maps `self`
/// to a `PlaceFlagKind` case for just the label lookup. Linux's
/// `PLACE_FLAG_BOXES` / android's `PlaceFlagBox` hand-roll the same match
/// again — out of scope for this apple-only lift.
enum PlaceFlagKind: CaseIterable {
    case originates, accepts, appliesDeletes

    var label: String {
        switch self {
        case .originates: L.devices.wizard.placeOriginates
        case .accepts: L.devices.wizard.placeAccepts
        case .appliesDeletes: L.devices.wizard.placeAppliesDeletes
        }
    }

    var desc: String {
        switch self {
        case .originates: L.devices.wizard.placeOriginatesDesc
        case .accepts: L.devices.wizard.placeAcceptsDesc
        case .appliesDeletes: L.devices.wizard.placeAppliesDeletesDesc
        }
    }
}

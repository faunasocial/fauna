import Foundation

/// Stored state of one photo-library asset in `PhotoBackupRecord` — the
/// PhotoKit-specific dedup ledger, which maps OS asset identity and therefore stays
/// in Swift (`file-sync.md` § Apple apps — convergence design, library-ingress
/// bullet).
///
/// This is **not** a sync-state store. Per-file sync state lives in the shared
/// engine's per-set `SyncDb` and is read through `FfiSyncEngineHost.fileStates`
/// (see `SyncStatesStore`); the SwiftData `SyncFile` model that used to mirror it —
/// and the `SyncAnchor` cursor beside it — retired with the B2 engine cutover. This
/// enum survives only because `PhotoBackupRecord` persists it.
public enum SyncFileState: String, Codable {
    case synced
    case localOnly
    case remoteOnly
    case uploading
    case downloading
    case conflict
}

public extension SyncFileState {
    /// The shared six-state display vocabulary (`fauna_core::format::SyncDisplayState`)
    /// for this stored state — a 1:1 map, since `SyncFileState` is exactly the six
    /// display states. Resolve any user-facing text through the shared
    /// `syncDisplayStateLabel(state:)` (`file-sync.md` § Per-file sync-status
    /// display); only a badge's icon/color is a per-app render.
    var displayState: SyncDisplayState {
        switch self {
        case .synced: .synced
        case .localOnly: .localOnly
        case .remoteOnly: .remoteOnly
        case .uploading: .uploading
        case .downloading: .downloading
        case .conflict: .conflict
        }
    }
}

import FaunaFFISwift
import Foundation

/// This row's `FolderRef` wire string — `Local(id)` for an own-nest row (owner or
/// same-nest member), `Foreign(channel)` for a cross-nest member row. One shared
/// rule (`folder_ref_for_row`, `on-demand-files.md` § Hosting multiple on-demand
/// folders — "A location binding identifies its set by a `FolderRef`, not by
/// name"; `nil` → the call site refuses whatever it was keying), so this is a
/// thin field-supply wrapper; the resolution semantics themselves are pinned
/// tier_1 in `engine_driver.rs`
/// (`engine_keys_for_prefers_the_ref_over_a_colliding_name`) — don't re-prove
/// them here, only that these three fields reach the call. Shared FaunaKit so the
/// macOS folder binding and both apple apps' File Provider domains key a row
/// the same way.
extension FolderSummary {
    public var folderRef: String? {
        folderRefForRow(id: id, mlsGroupIdHex: mlsGroupId, homeNestUrl: homeNestUrl)
    }
}

extension FolderResponse {
    /// The `FolderRef` wire string of a `fauna.folders.list` row. That projection
    /// is this nest's own table (it carries no `home_nest_url`), so the row
    /// always takes the `Local(id)` arm — through the same shared rule, never a
    /// hand-spelled `local:` prefix. The File Provider domain reconcile keys
    /// each own set's domain by it.
    public var folderRef: String? {
        folderRefForRow(id: Int64(id), mlsGroupIdHex: nil, homeNestUrl: nil)
    }
}

import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `serve_enable_folder` — arrange a WebDAV-served, content-keyed folder for the
/// logged-in actor (optionally creating it first): `APIClient.createFolder`
/// (mode "sync", matching linux's `FolderCreateRequest`) then
/// `APIClient.serveFolderWebdav` (owner-only — `mlsGroupIdHex: nil` — the common
/// test case), the same `FoldersAuthor::serve_set` UniFFI face the WebDAV
/// `folder-webdav-toggle` UI drives. Writes the outcome into
/// `TestAgentReplies.webdavServeReply` (`tests/e2e-unified/helpers/
/// webdav_roundtrip.py::serve_enable_folder` polls it — the same
/// `{"ok": true, "served_sets": N}` shape linux serializes).
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2) — it
/// was a macOS-only inline handler in `FaunaMacApp.swift` until this pass, which
/// is exactly why iOS could not witness `files-in-standard-apps` outcome 2: the
/// production control it stands in for (the shared `FolderWebdavToggle` in
/// `FoldersContent`) has been on both platforms since 2026-07-12, so the gap was
/// never a missing control, only a missing test seam. Takes `client` directly rather than
/// `AppState`/`MacAppState` (no common protocol between them), like
/// `CaldavMailboxTestCommand`.
public enum ServeEnableFolderTestCommand {
    @MainActor
    public static func apply(_ command: [String: Any], client: FaunaClient?) async {
        TestAgentReplies.webdavServeReply = nil
        guard let api = client?.api else {
            TestAgentReplies.webdavServeReply = ["ok": false, "error": "fauna client not initialized"]
            return
        }
        let set = command["folder"] as? String ?? ""
        let create = command["create"] as? Bool ?? true
        do {
            if create {
                _ = try await api.createFolder(name: set)
            }
            let served = try await api.serveFolderWebdav(name: set, mlsGroupIdHex: nil, enable: true)
            TestAgentReplies.webdavServeReply = ["ok": true, "served_sets": served]
        } catch {
            TestAgentReplies.webdavServeReply = ["ok": false, "error": "\(error)"]
        }
    }
}

#endif

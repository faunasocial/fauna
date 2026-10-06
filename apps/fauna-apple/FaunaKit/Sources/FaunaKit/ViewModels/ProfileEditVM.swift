import SwiftUI

/// View-model for the profile **edit form** (`profile.md` § Where logic lives →
/// *Profile publish/edit* + *Field ownership*), shared by macOS + iOS (one
/// FaunaKit VM). Drives the read-modify-write: `profileGet` → `decodeProfile`
/// (populate the three text fields) → user edits (text + a staged
/// avatar/banner picker) → `buildEditedProfileWithImages` (sign, preserving the
/// non-display fields, resolving the avatar/banner three-state edit) →
/// `profileSet`. The read-modify-write + the Ed25519 sign live once in shared
/// Rust (`fauna-client-profile`); this VM only orchestrates the UI + the
/// picture upload (the ordinary public-post blob path, mirroring
/// `FeedVM.attachComposeFile`). Mirrors linux
/// `apps/fauna-linux/src/views/profile/edit.rs` + android `ProfileEditVM`.
///
/// Observer-free: the shell re-reads the header `display_name` (via `profileGet`)
/// through the `onSaved` callback after a successful publish.
@MainActor @Observable
public final class ProfileEditVM {
    /// The inline form is visible. Revealed only AFTER the base-profile fetch
    /// completes (so the read-modify-write base is captured before editing), which
    /// is why the e2e waits for `profile-edit-form` after clicking the edit button.
    public private(set) var isOpen = false
    public var displayName = ""
    public var bio = ""
    /// Repeatable label/uri rows (`profile-edit-link-list`). `var` so the form can
    /// bind into each element's fields.
    public var links: [FfiProfileLink] = []
    /// Shared page-level error (`error-message`) on a failed publish.
    public var errorMessage: String?

    /// Avatar/banner three-state edit (profile.md § Where logic lives → Field
    /// ownership): a staged pick (`avatarStagedPath` display text +
    /// `avatarStagedData` the already-read bytes) uploads through the ordinary
    /// public-post blob path on Save → `.set`; `avatarCleared` with no staged
    /// pick → `.clear`; neither → `.keep`. Picking a new file always wins over a
    /// prior Clear (mirrors linux `pending_image`'s last-write shape). Bytes are
    /// read at PICK time, not deferred to Save: a user-picked file is
    /// security-scoped (`ComposeAttachButton`'s same constraint), so the read
    /// must happen while that scope is open — ProfileEditFormView/the TestAgent
    /// path both read immediately and hand this VM the resulting `Data`.
    public private(set) var avatarStagedPath: String?
    private var avatarStagedData: Data?
    public private(set) var avatarCleared = false
    public private(set) var bannerStagedPath: String?
    private var bannerStagedData: Data?
    public private(set) var bannerCleared = false

    /// The stored profile bytes the edit started from (`nil` = first publish); the
    /// non-display fields ride through `buildEditedProfile`'s read-modify-write.
    private var baseBody: Data?
    private var api: APIClient?

    public init() {}

    /// Open the form for `actorId` (the SELF actor): fetch the current profile,
    /// populate the editable fields, then reveal. A missing profile (first
    /// publish) or decode failure starts blank.
    public func open(api: APIClient, actorId: String) async {
        self.api = api
        errorMessage = nil
        do {
            let body = try await api.loadProfileEditBase()
            baseBody = body
            if let body {
                let display = try api.decodeProfile(body: body)
                displayName = display.displayName ?? ""
                bio = display.bio ?? ""
                links = display.links
            } else {
                displayName = ""
                bio = ""
                links = []
            }
        } catch {
            // read/decode failure → first publish: start blank.
            baseBody = nil
            displayName = ""
            bio = ""
            links = []
        }
        avatarStagedPath = nil
        avatarStagedData = nil
        avatarCleared = false
        bannerStagedPath = nil
        bannerStagedData = nil
        bannerCleared = false
        isOpen = true
    }

    public func cancel() {
        isOpen = false
        errorMessage = nil
    }

    public func addLink() {
        links.append(FfiProfileLink(label: "", uri: ""))
    }

    public func removeLink(at index: Int) {
        guard links.indices.contains(index) else { return }
        links.remove(at: index)
    }

    /// Stage a picked avatar: `path` is display-only (the `profile-edit-avatar`
    /// button's readback text); `data` is the already-read bytes uploaded on Save.
    public func stageAvatar(path: String, data: Data) {
        avatarStagedPath = path
        avatarStagedData = data
        avatarCleared = false
    }

    public func clearAvatar() {
        avatarStagedPath = nil
        avatarStagedData = nil
        avatarCleared = true
    }

    /// Stage a picked banner — see `stageAvatar`.
    public func stageBanner(path: String, data: Data) {
        bannerStagedPath = path
        bannerStagedData = data
        bannerCleared = false
    }

    public func clearBanner() {
        bannerStagedPath = nil
        bannerStagedData = nil
        bannerCleared = true
    }

    /// Build the signed edited profile (preserving the non-display fields) and
    /// publish it. On success hide the form and run `onSaved` (the shell re-reads
    /// the header `display_name`). Fully-empty link rows are dropped (mirror linux
    /// `collect_links`).
    public func save(onSaved: () async -> Void) async {
        guard let api else { return }
        let cleanLinks = links.filter {
            !$0.label.trimmingCharacters(in: .whitespaces).isEmpty
                || !$0.uri.trimmingCharacters(in: .whitespaces).isEmpty
        }
        do {
            let avatar = try await resolveImageEdit(
                stagedData: avatarStagedData, cleared: avatarCleared, api: api)
            let banner = try await resolveImageEdit(
                stagedData: bannerStagedData, cleared: bannerCleared, api: api)
            let body = try api.buildEditedProfileWithImages(
                baseBody: baseBody,
                displayName: optTrimmed(displayName),
                bio: optTrimmed(bio),
                links: cleanLinks,
                avatar: avatar,
                banner: banner)
            try await api.profileSet(body: body)
            errorMessage = nil
            isOpen = false
            await onSaved()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Resolve one field's staged state to the wire three-state edit, uploading
    /// newly-staged bytes through the ordinary public-post blob path (mirrors
    /// `FeedVM.attachComposeFile` — same seal+multipart pipeline, `.publicPost`
    /// audience: a profile picture is a public field, never sealed under a
    /// per-post/library key).
    private func resolveImageEdit(
        stagedData: Data?, cleared: Bool, api: APIClient
    ) async throws -> FfiProfileImageEdit {
        if let stagedData {
            let response = try await api.uploadBlob(data: stagedData, audience: .publicPost)
            return .set(blobHashHex: response.hash)
        }
        return cleared ? .clear : .keep
    }
}

/// Trim + collapse an empty field to `nil` (the optional wire shape).
private func optTrimmed(_ s: String) -> String? {
    let t = s.trimmingCharacters(in: .whitespaces)
    return t.isEmpty ? nil : t
}

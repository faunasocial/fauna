import SwiftUI

/// The Profile header's stored avatar and banner (`profile.md` § Layout & flow, the
/// `user-header` component; `profile-avatar` / `profile-banner`).
public enum ProfileHeaderPictures {
    #if !FAUNA_KIDS  // rides the post-blob plane, which the kids flavor lacks
    /// The post-blob home a header picture is fetched from: `nil` (this nest) when
    /// this nest served the profile, else the `relayed_from` home paired with the
    /// identity the relay reply stamped (`HeaderRead::home_nest_actor_id`). Never
    /// derived from the profile's own `nests` — routing, not a transport anchor
    /// (`transport-trust.md` § The post arm → *A relayed profile's pictures*).
    public static func home(for read: FfiProfileHeaderRead?) -> FfiPostBlobHome? {
        guard let url = read?.relayedFrom else { return nil }
        return FfiPostBlobHome(url: url, nestActorId: read?.homeNestActorId)
    }
    #endif
}

/// One header picture leaf. Rendered only when the profile names the picture;
/// holds no pixels while loading or after a failed fetch. Answers
/// `get_attr(<id>, state)` = `painted` / `placeholder`, `post-image`'s contract.
struct ProfileHeaderPicture: View {
    let id: String
    let image: FaunaPlatformImage?

    var body: some View {
        let state = image == nil ? "placeholder" : "painted"
        Group {
            if let image {
                Image(platformImage: image).resizable().scaledToFill()
            } else {
                Color.secondary.opacity(0.15)
            }
        }
        .clipped()
        .accessibilityIdentifier(id)
        .automationValue(id, text: { nil }, value: { state })
    }
}

import SwiftUI

/// The two shared capsule-pill badge shapes every apple badge view uses
/// (priority #1/#2/#4) — extracted from byte-identical copies of each
/// modifier chain (`DeviceFolderRoleBadge`/`ContactStatusBadge` for the tinted
/// pill; `UnverifiedSourceBadge`/
/// `GatedPostBadge`/`DelegatedOriginBadge` for the muted caveat pill) so a
/// future badge picks the shape up instead of hand-rolling a fourth copy.
extension View {
    /// The bold, uppercase, color-tinted capsule pill — a status/role/mode
    /// label whose color carries meaning (e.g. contact status); also worn in a
    /// neutral tint by the folder-place chip.
    public func tintedCapsuleBadge(_ color: Color) -> some View {
        self
            .font(.caption2)
            .fontWeight(.semibold)
            .textCase(.uppercase)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.15))
            .foregroundStyle(color)
            .clipShape(Capsule())
    }

    /// The muted secondary capsule pill — post-card chrome flagging an
    /// out-of-band signal (unverified signature, gated tier, delegated
    /// origin) that the reader should notice without it competing with the
    /// post's own color-coded status.
    public func secondaryCaveatBadge() -> some View {
        self
            .font(.caption2)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(.secondary.opacity(0.12))
            .clipShape(Capsule())
    }
}

import Foundation

/// Where a notification row goes when it is opened — the ONE call into shared
/// Rust's `notification_destination` that both apple shells make
/// (`docs/goal/behavior/notifications.md` § Deep-link destinations). The twin
/// of `FeedPostCardOpen.swift` for this page.
///
/// **No app may substitute its own `notifType` match for this.** The decision is
/// keyed on `source` **then** `notifType`, because a bridged row reuses the
/// native type vocabulary while its `contentId` is a dedup token — matching the
/// type alone deep-links every bridged like to a post that cannot exist. Reading
/// the raw wire fields back off `NotificationItem` and handing them to the
/// router is what keeps that trap in one place (priorities #2/#3).
///
/// `hasFamily` gates the `Family` arm exactly as tui's own `navigable` does:
/// both apple targets reach the Family surface only when the caller has a
/// `fauna.family.status` relationship (macOS's gated sidebar row, iOS's gated
/// Settings entry), so a doorbell that outlived its family link would otherwise
/// offer a control leading to a page with no way in.
///
/// The `External` arm — a bridged Bluesky row's post on bsky.app — is always
/// actionable: each shell hands its URL to `OpenURL.open`, the same opener
/// every other external link uses (fire-and-forget, e2e-suppressed).
///
/// Exhaustive on purpose — no `default` arm — so a new variant on the shared
/// enum has to be decided here rather than silently painting a dead control
/// (tui's `navigable` rule, and search's before it).
///
/// `nil` means the row is honestly inert and must render as a plain **label**,
/// never as a disabled control: the automation server answers HTTP 409 for a
/// disabled control, so a registered-but-disabled activate would *refuse* the
/// gesture where the page's contract is to ignore it.
public func notificationDestination(
    for item: NotificationItem, hasFamily: Bool
) -> FfiNotificationDestination? {
    guard let ffi = item.ffiItem else { return nil }
    guard let routed = notificationDestinationFor(item: ffi) else { return nil }
    switch routed {
    case .post, .knock, .external: return routed
    case .family: return hasFamily ? routed : nil
    }
}

/// Whether the row collapses behind `notification-muted` — the shared
/// `notification_muted` decision (`docs/goal/behavior/notifications.md`
/// § Muted words: the relayed `body.args` matched at Hide, a `security.notice`
/// never) over the row as it arrived. `mutedKeywords` is the process-held list
/// (``MutedWordsHolder``), never a read of its own. A row whose id will not
/// round-trip renders in full.
public func notificationMuted(_ item: NotificationItem, mutedKeywords: [MutedKeyword]) -> Bool {
    guard !mutedKeywords.isEmpty, let ffi = item.ffiItem else { return false }
    return notificationMutedFor(item: ffi, mutedKeywords: mutedKeywords)
}

extension NotificationItem {
    /// The row handed back to shared Rust as the wire shape it arrived in —
    /// the one reconstruction the router and the muted-word decision share.
    /// `nil` when the stringified id will not round-trip.
    var ffiItem: FfiNotifItem? {
        // `NotificationItem.id` is the wire id stringified at the FFI seam; a
        // row whose id will not round-trip is one this build cannot act on.
        guard let id = Int64(id) else { return nil }
        return FfiNotifItem(
            id: id,
            notifType: notifType,
            source: source,
            senderId: senderId,
            contentId: contentId,
            // A bridged row's subject as an AT-URI — the `External` arm's
            // input; shared Rust turns it into the post's bsky.app address.
            subjectUri: subjectUri,
            // The row's real English summary, not `body` (the already-resolved
            // localized/verbatim text) — the muted decision's fallback for a
            // row with no `body`, so the field must say what it claims to.
            summary: summary,
            isRead: read,
            createdAt: Int64(createdAt),
            body: localizedBody
        )
    }
}

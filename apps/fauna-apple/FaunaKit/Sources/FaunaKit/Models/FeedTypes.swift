import Foundation

// The Feed page's post-list / feed-definition / filter-rule / quoted-post types
// moved to the shared `fauna_feed` snapshot (`FeedSnapshot` / `PostSummary` /
// `FeedSummaryView` / `FilterRuleInput` / `QuotedPostView`, consumed via the
// `FfiFeedManager` façade — the 2026-06-16 lift). The old HTTP-era FaunaKit
// structs (`FeedDefinition` / `FeedPost` / `FeedPostsResponse` / `FilterRule` /
// `LinkPreview` / `QuotedPostData`) + their `FeedFFIMapping` were removed.
//
// What survives here is the bridge-feed subscription row — a distinct
// `fauna.bridges.feeds.*` type mapped by `BridgeFFIMapping.swift`.

public struct BridgeFeedSubscription: Codable, Identifiable {
    public let id: String
    public let bridge: String
    public let feedUri: String
    public let name: String
    public let createdAt: Int

    enum CodingKeys: String, CodingKey {
        case id, bridge, name
        case feedUri = "feed_uri"
        case createdAt = "created_at"
    }
}

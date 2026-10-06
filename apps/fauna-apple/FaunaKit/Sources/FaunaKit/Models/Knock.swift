import Foundation

public struct Knock: Codable, Identifiable {
    public let id: Int
    public let sender: String
    public let senderNode: String?
    public let summary: String?
    public let createdAt: Int

    enum CodingKeys: String, CodingKey {
        case id, sender, summary
        case senderNode = "sender_node"
        case createdAt = "created_at"
    }
}

public struct Contact: Codable, Identifiable {
    public let peerId: String
    public let status: String
    public let updatedAt: Int?
    /// The peer's public handle and this nest's handle domain — `Some` for a
    /// local peer with a handle, `nil` for a federated peer (the nest holds no
    /// cached federated Profile). Feed the shared roster filter and the row's
    /// names (`ContactsVM.rosterGroups`; contacts.md § Contact roster filter).
    public let handle: String?
    public let domain: String?

    public var id: String { peerId }

    enum CodingKeys: String, CodingKey {
        case status, handle, domain
        case peerId = "peer_id"
        case updatedAt = "updated_at"
    }
}

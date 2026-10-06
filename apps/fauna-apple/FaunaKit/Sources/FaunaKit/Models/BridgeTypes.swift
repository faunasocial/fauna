import Foundation

public struct BridgeInfo: Codable, Identifiable {
    public var id: String
    public let name: String
    public let available: Bool
    public let linked: Bool
    public let identity: BridgeIdentity?
    public let mode: String?
    public let settings: [BridgeSetting]
    public let supportsFollows: Bool
    public let linkModes: [BridgeLinkMode]?
    /// The nest's own explanation when `provider.status()` errored — the
    /// degraded shape's `error` slot (bridges.md § Errors & edge cases). Was
    /// absent from this struct, so apple decoded the wire and threw the
    /// sentence away, leaving a user with a dead Link control and no reason.
    public let error: String?

    enum CodingKeys: String, CodingKey {
        case id, name, available, linked, identity, mode, settings, error
        case supportsFollows = "supports_follows"
        case linkModes = "link_modes"
    }
}

public struct BridgeIdentity: Codable {
    public let label: String
    public let value: String
    public let display: String
}

public struct BridgeSetting: Codable, Identifiable {
    public var id: String { key }
    public let key: String
    public let label: String
    public let type: String
    public let value: AnyCodable
    public let options: [BridgeSettingOption]?
}

public struct BridgeSettingOption: Codable {
    public let value: AnyCodable
    public let label: String
}

public struct BridgeLinkMode: Codable, Identifiable {
    public var id: String { mode }
    public let mode: String
    public let label: String
    public let clientAction: String?
    public let platform: String?
    public let fields: [BridgeLinkField]

    enum CodingKeys: String, CodingKey {
        case mode, label, fields, platform
        case clientAction = "client_action"
    }
}

public struct BridgeLinkField: Codable, Identifiable {
    public var id: String { key }
    public let key: String
    public let label: String
    public let type: String
    public let placeholder: String?
}

public struct BridgeLinkResponse: Codable {
    public let linked: Bool
    public let identity: BridgeIdentity?
    public let redirectUrl: String?

    enum CodingKeys: String, CodingKey {
        case linked, identity
        case redirectUrl = "redirect_url"
    }
}

public struct BridgeFollow: Codable, Identifiable {
    public var id: String
    public let petname: String?
    public let createdAt: Int?

    enum CodingKeys: String, CodingKey {
        case id, petname
        case createdAt = "created_at"
    }
}

/// Type-erased Codable wrapper for mixed JSON values.
public struct AnyCodable: Codable {
    public let value: Any

    public init(_ value: Any) { self.value = value }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let b = try? container.decode(Bool.self) { value = b }
        else if let i = try? container.decode(Int.self) { value = i }
        else if let d = try? container.decode(Double.self) { value = d }
        else if let s = try? container.decode(String.self) { value = s }
        else { value = "" }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        if let b = value as? Bool { try container.encode(b) }
        else if let i = value as? Int { try container.encode(i) }
        else if let d = value as? Double { try container.encode(d) }
        else if let s = value as? String { try container.encode(s) }
    }

    public var boolValue: Bool? { value as? Bool }
    public var stringValue: String? { value as? String }
    public var intValue: Int? { value as? Int }
}

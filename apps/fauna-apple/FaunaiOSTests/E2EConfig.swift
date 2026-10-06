import Foundation

struct E2EConfig: Codable {
    struct User: Codable {
        let secretHex: String
        let actorId: String
        let handle: String
        let deviceId: String
    }
    struct Recipient: Codable {
        let actorId: String
        let nodeUrl: String
    }
    let nodeUrl: String
    let activeUser: User
    let messageRecipient: Recipient?

    static let path = "/tmp/fauna-e2e-ios-config.json"

    static func load() -> E2EConfig {
        let url = URL(fileURLWithPath: Self.path)
        let data = try! Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try! decoder.decode(E2EConfig.self, from: data)
    }
}

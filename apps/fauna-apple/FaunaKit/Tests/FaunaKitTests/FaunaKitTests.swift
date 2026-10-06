import Testing
@testable import FaunaKit

@Test func sessionStateDefaults() async throws {
    let state = SessionState()
    #expect(state.isAuthenticated == false)
    #expect(state.secretHex == nil)
    #expect(state.actorId == nil)
    #expect(state.nodeUrl == nil)
    #expect(state.deviceId == nil)
}

@Test func syncConfigurationPlatformDefault() async throws {
    let config = SyncConfiguration.platformDefault
    #expect(config.maxConcurrentUploads > 0)
    #expect(config.maxInMemoryFileSize > 0)
}

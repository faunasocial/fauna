import XCTest

final class SettingsE2ETests: XCTestCase {
    private var app: XCUIApplication!
    private var config: E2EConfig!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        config = E2EConfig.load()
        app = AppHelper.launchApp()
    }

    override func tearDown() {
        app.terminate()
        super.tearDown()
    }

    func testSettingsShowIdentity() {
        AppHelper.onboard(app: app, config: config)

        let settingsTab = app.tabBars.buttons["Settings"]
        settingsTab.tap()

        let handleLabel = app.staticTexts["settings-handle-label"]
        handleLabel.waitForLabel(containing: config.activeUser.handle)

        app.buttons["account-settings-link"].waitAndTap()

        let actorIdText = app.staticTexts["account-actor-id"]
        XCTAssertTrue(actorIdText.waitForExistence(timeout: 5), "Expected actor ID to be visible in account settings")

        let prefix = String(config.activeUser.actorId.prefix(12))
        actorIdText.waitForLabel(containing: prefix)
    }
}

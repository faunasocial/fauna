import XCTest

enum AppHelper {
    /// Launch the app fresh. Returns the XCUIApplication instance.
    static func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launch()
        return app
    }

    /// Perform the full onboarding flow using credentials from config.
    /// Returns the app instance positioned on the main tab view.
    @discardableResult
    static func onboard(app: XCUIApplication, config: E2EConfig) -> XCUIApplication {
        let user = config.activeUser

        // Welcome screen → tap Sign In
        app.buttons["sign-in-button"].waitAndTap()

        // Enter secret key
        let secretField = app.textFields["secret-key-field"]
        secretField.waitAndTap()
        secretField.typeText(user.secretHex)

        // Submit sign-in
        app.buttons["sign-in-submit-button"].waitAndTap()

        // Node picker → enter custom node URL
        let nodeField = app.textFields["node-url-field"]
        nodeField.waitAndTap()
        nodeField.typeText(config.nodeUrl)
        app.buttons["use-custom-node-button"].waitAndTap()

        // Handle picker → enter handle and register
        let handleField = app.textFields["handle-field"]
        handleField.waitAndTap()
        handleField.typeText(user.handle)
        app.buttons["register-button"].waitAndTap(timeout: 15)

        // Sync setup → skip
        app.buttons["skip-sync-button"].waitAndTap(timeout: 10)

        // Assert main tab view appeared
        let tabView = app.otherElements["main-tab-view"]
        XCTAssertTrue(tabView.waitForExistence(timeout: 10), "Main tab view did not appear after onboarding")

        return app
    }
}

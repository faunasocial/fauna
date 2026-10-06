import XCTest

final class MessagingE2ETests: XCTestCase {
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

    /// Called by Python to onboard Alice (first step of messaging test).
    func testOnboardAlice() {
        AppHelper.onboard(app: app, config: config)

        let messagesTab = app.tabBars.buttons["Messages"]
        XCTAssertTrue(messagesTab.exists, "Messages tab should be visible")
    }

    /// Called by Python after resetting app and switching active_user to Bob.
    func testBobSendsMessage() {
        // Onboard Bob
        AppHelper.onboard(app: app, config: config)

        // Navigate to Messages tab
        let messagesTab = app.tabBars.buttons["Messages"]
        messagesTab.tap()

        // Tap compose
        app.buttons["compose-button"].waitAndTap()

        // Fill in recipient
        guard let recipient = config.messageRecipient else {
            XCTFail("message_recipient not set in config")
            return
        }

        let recipientField = app.textFields["recipient-id-field"]
        recipientField.waitAndTap()
        recipientField.typeText(recipient.actorId)

        let nodeUrlField = app.textFields["dm-recipient-node-url"]
        nodeUrlField.tap()
        nodeUrlField.typeText(recipient.nodeUrl)

        // Fill in message
        let subjectField = app.textFields["dm-subject-field"]
        subjectField.tap()
        subjectField.typeText("E2E Test Message")

        let bodyField = app.textViews["message-body-field"]
        bodyField.tap()
        bodyField.typeText("Hello from Bob via e2e test")

        // Send
        app.buttons["send-button"].waitAndTap()

        // Verify compose sheet dismissed — Messages tab should still be visible
        XCTAssertTrue(messagesTab.waitForExistence(timeout: 5), "Should return to message list after sending")
    }
}

import XCTest

final class FileSyncE2ETests: XCTestCase {
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

    func testFileAppearsAfterSync() {
        // Onboard
        AppHelper.onboard(app: app, config: config)

        // Navigate to Files tab
        let filesTab = app.tabBars.buttons["Files"]
        filesTab.tap()

        // Tap the first folder row (documents)
        let documentsRow = app.buttons.matching(identifier: "folder-row").element(boundBy: 0)
        documentsRow.waitAndTap()

        // Wait for file list to contain the seeded file
        let fileRow = app.buttons["sync-file-row"]
        XCTAssertTrue(
            fileRow.waitForExistence(timeout: 15),
            "Expected at least one file to appear in the documents folder after sync"
        )

        // Verify the file name is visible
        let fileText = app.staticTexts["test-file.txt"]
        XCTAssertTrue(
            fileText.waitForExistence(timeout: 5),
            "Expected 'test-file.txt' to be visible in the file list"
        )
    }
}

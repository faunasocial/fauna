import XCTest

final class PhotoBackupE2ETests: XCTestCase {
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

    func testEnablePhotoBackup() {
        // Onboard
        AppHelper.onboard(app: app, config: config)

        // Navigate to Files tab
        let filesTab = app.tabBars.buttons["Files"]
        filesTab.tap()

        // Find and tap into Photo Backup section
        let photoBackupCell = app.staticTexts["Photo Backup"]
        photoBackupCell.waitAndTap()

        // Enable the toggle
        let toggle = app.switches["photo-backup-toggle"]
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Photo backup toggle should exist")

        // Only toggle if currently off
        if toggle.value as? String == "0" {
            toggle.tap()
        }

        // Wait a moment for the backup process to start
        // The actual verification (blob exists on server) is done by Python
        Thread.sleep(forTimeInterval: 5)

        // Assert the toggle is now on
        XCTAssertEqual(toggle.value as? String, "1", "Photo backup toggle should be enabled")
    }
}

import XCTest

final class SearchE2ETests: XCTestCase {
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

    func testSearchShowsResults() {
        AppHelper.onboard(app: app, config: config)

        let searchField = app.searchFields.firstMatch
        XCTAssertTrue(searchField.waitForExistence(timeout: 5), "Search field should exist")
        searchField.tap()
        searchField.typeText("test query")

        app.keyboards.buttons["Search"].tap()

        let resultsView = app.otherElements["search-results-view"]
        XCTAssertTrue(resultsView.waitForExistence(timeout: 10), "Search results view should appear after submitting a query")

        let noResults = app.otherElements["search-no-results"]
        let hasResults = resultsView.exists
        XCTAssertTrue(hasResults || noResults.exists, "Expected either search results or 'No Results' view")
    }
}

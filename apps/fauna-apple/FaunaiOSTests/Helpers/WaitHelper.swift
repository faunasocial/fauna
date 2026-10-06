import XCTest

extension XCUIElement {
    /// Wait for the element to exist and then tap it.
    func waitAndTap(timeout: TimeInterval = 10, file: StaticString = #file, line: UInt = #line) {
        let exists = self.waitForExistence(timeout: timeout)
        XCTAssertTrue(exists, "Element \(self) did not appear within \(timeout)s", file: file, line: line)
        self.tap()
    }

    /// Wait until the element's label contains the expected text.
    func waitForLabel(containing text: String, timeout: TimeInterval = 10, file: StaticString = #file, line: UInt = #line) {
        let predicate = NSPredicate(format: "label CONTAINS[c] %@", text)
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: self)
        let result = XCTWaiter().wait(for: [expectation], timeout: timeout)
        XCTAssertEqual(result, .completed, "Element label did not contain '\(text)' within \(timeout)s", file: file, line: line)
    }
}

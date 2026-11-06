// The iOS store driver — obtains the app from TestFlight on a physical device
// and drives the install TestFlight delivered, from OUTSIDE the app.
//
// Ruling: docs/goal/architecture/feature-catalog.md § Implementation status
// today, the bullet on how the iOS store install is driven. Convention 15
// compiles the in-process automation surface (the `AutomationRegistry`, the
// TestAgent and its launch seams) out of every Release build, and a TestFlight
// delivery is a Release build Apple re-signed, so nothing inside the app can be
// reached. This bundle is a separate XCUITest runner with its own bundle
// identifier and signing; it reaches the app only through the accessibility
// tree, by `XCUIApplication(bundleIdentifier:)`, through the identifiers the
// views set with `.accessibilityIdentifier` outside any `#if DEBUG` (the ids
// come from the generated `Ids` table, compiled in from FaunaKit's source, so
// the driver and the app can never disagree about a string).
//
// It has none of the harness's in-process powers — no credential seed, no log
// read, no launch environment — so the journey signs in through the app's own
// UI. The host half (`tests/e2e-unified/helpers/ios_store_driver.py`) runs one
// test method per call with `xcodebuild test-without-building` and reads the
// outcome off the result bundle: a skip's message starts with `state=<word>`,
// which the host maps to an environment skip; anything else is a failure.

import XCTest

/// The app's store identity (installers/ios.md § Store identity).
private let faunaBundleID = "social.fauna.fauna"
/// Apple's TestFlight app — the installer of record a TestFlight install names.
private let testFlightBundleID = "com.apple.TestFlight"

/// Inputs, passed by the host as `TEST_RUNNER_<NAME>` (xcodebuild strips the
/// prefix into the runner's environment). Harness inputs, never product settings.
private enum Input {
    static var env: [String: String] { ProcessInfo.processInfo.environment }
    /// The name TestFlight lists the app under (the on-device name, `Fauna`).
    static var appName: String { env["FAUNA_STORE_APP_NAME"].flatMap { $0.isEmpty ? nil : $0 } ?? "Fauna" }
    /// The account the sign-in leg imports: the store's reviewer account on the
    /// staging box (`apple-reviewer` on `test.fauna.build`), read by the host
    /// from the identity file the provisioning drive keeps
    /// (`tests/e2e-unified/helpers/store_install_account.py`). The handle, and its
    /// 64-hex identity seed — never echoed into a failure message.
    static var accountHandle: String { trimmed("FAUNA_STORE_ACCOUNT_HANDLE") }
    static var accountSecret: String { trimmed("FAUNA_STORE_ACCOUNT_SECRET") }
    private static func trimmed(_ name: String) -> String {
        (env[name] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    }
    /// How long TestFlight's download and install may take after Install.
    static var installBudget: TimeInterval { TimeInterval(env["FAUNA_STORE_INSTALL_BUDGET_S"] ?? "") ?? 600 }
}

final class StoreDriverTests: XCTestCase {
    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    // MARK: - obtain

    /// Open TestFlight at the app's entry and press Install, then wait until
    /// TestFlight itself shows the app as installed (its Open button). The
    /// host asserts beforehand that the app was absent, and afterwards reads the
    /// device's own classification of the install; this method's part is the
    /// tester's tap, and TestFlight's own record that the install completed.
    func testObtainFromTestFlight() throws {
        let testFlight = XCUIApplication(bundleIdentifier: testFlightBundleID)
        testFlight.activate()
        guard testFlight.wait(for: .runningForeground, timeout: 30) else {
            throw XCTSkip("state=testflight_absent TestFlight never came to the foreground — is it installed on the device?")
        }
        if signedOut(testFlight) {
            throw XCTSkip("state=signed_out TestFlight on the device is signed out — the one-time sign-in to a tester's Apple Account is the owner's step")
        }
        let entry = testFlight.staticTexts.matching(NSPredicate(format: "label == %@", Input.appName)).firstMatch
        guard entry.waitForExistence(timeout: 30) else {
            throw XCTSkip("state=not_available TestFlight lists no app named \(Input.appName) for the signed-in tester; visible: \(visibleTexts(testFlight))")
        }
        entry.tap()

        let open = button(testFlight, labels: ["Open", "OPEN"])
        if open.waitForExistence(timeout: 10) {
            // Already installed: the host refuses to adopt an install it did not
            // see TestFlight make, so reaching here means the host already
            // checked the installer; report and stop.
            print("STORE-DRIVER state=installed")
            return
        }
        let install = button(testFlight, labels: ["Install", "INSTALL", "Update", "UPDATE"])
        guard install.waitForExistence(timeout: 30) else {
            XCTFail("TestFlight's page for \(Input.appName) shows no Install button; visible: \(visibleTexts(testFlight))")
            return
        }
        install.tap()
        print("STORE-DRIVER state=pressed")
        // TestFlight may ask once whether to share crash data / accept terms.
        for label in ["Accept", "Next", "Start Testing", "Don't Share", "Share"] {
            let b = testFlight.buttons[label]
            if b.waitForExistence(timeout: 3) { b.tap() }
        }
        XCTAssertTrue(
            open.waitForExistence(timeout: Input.installBudget),
            "Install was pressed but TestFlight never showed \(Input.appName) installed (Open) within \(Int(Input.installBudget)) s; visible: \(visibleTexts(testFlight))"
        )
        print("STORE-DRIVER state=installed")
    }

    // MARK: - the app, from outside

    /// `get-the-app` outcome 1, the phone arm, the drive half: the installed
    /// app opens and you are signed in — through the app's own onboarding UI,
    /// as the App Store's reviewer does from the review notes: import the
    /// identity, type the handle, continue. The same steps as the android store
    /// witness. A fresh identity cannot reach the feed: the release onboarding
    /// registers only with an invite code, and the organisation's nests are
    /// invite-only (feature-catalog.md § Implementation status today).
    func testSignInToFeed() throws {
        let handle = Input.accountHandle
        let secret = Input.accountSecret
        guard !handle.isEmpty, secret.count == 64 else {
            throw XCTSkip("state=no_account the host passed no reviewer account (FAUNA_STORE_ACCOUNT_HANDLE and a 64-hex FAUNA_STORE_ACCOUNT_SECRET) — the sign-in leg imports the apple-reviewer account on test.fauna.build")
        }
        let app = XCUIApplication(bundleIdentifier: faunaBundleID)
        app.terminate()
        app.activate()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 60),
                      "the installed app never came to the foreground")

        // The release build opens on onboarding's identity choice. Import the
        // reviewer's identity on the paste tab (the import view's default).
        tap(app, Ids.importIdentityButton, timeout: 60)
        let secretField = element(app, Ids.pasteSecretField)
        XCTAssertTrue(secretField.waitForExistence(timeout: 15), diagnose(app, Ids.pasteSecretField))
        secretField.tap()
        secretField.typeText(secret)
        tap(app, Ids.importSubmitButton, timeout: 10)

        // The import may pre-fill the handle: clear the field before typing.
        let field = element(app, Ids.handleInput)
        XCTAssertTrue(field.waitForExistence(timeout: 30), diagnose(app, Ids.handleInput))
        clearAndType(field, handle)
        tap(app, Ids.handleCheckButton, timeout: 10)

        let next = element(app, Ids.handleEntryContinueButton)
        let enabled = NSPredicate(format: "exists == true AND enabled == true")
        let ready = XCTNSPredicateExpectation(predicate: enabled, object: next)
        XCTAssertEqual(XCTWaiter().wait(for: [ready], timeout: 60), .completed,
                       "'\(Ids.handleEntryContinueButton)' never enabled — the handle check did not pass; "
                       + "handle-message-area: \(element(app, Ids.handleMessageArea).label)")
        next.tap()

        // Signed in: the main tab view is on screen (the android witness waits
        // for `feed-tab`). On iOS the `{page}-tab` ids are registered with the
        // in-process registry only (a SwiftUI tab item takes no accessibility
        // identifier; `test_ios_store_driver_pins.py` refuses a driver that
        // reads one), so the release tree is read at the container the view
        // does identify, `main-tab-view`, or at the tab bar it hosts —
        // whichever the first device run finds, it reports.
        let tabView = element(app, Ids.mainTabView)
        let tabBar = app.tabBars.firstMatch
        let deadline = Date().addingTimeInterval(180)
        while Date() < deadline {  // deadline-ok: waits on the signed-in container's presence, a state
            if tabView.exists { print("STORE-DRIVER signed_in=main-tab-view"); return }
            if tabBar.exists { print("STORE-DRIVER signed_in=tab-bar"); return }
            _ = tabView.waitForExistence(timeout: 2)
        }
        XCTFail("the store-installed app never reached the main tab view after signing in as \(handle); "
                + "error element: \(errorText(app))")
    }

    // MARK: - helpers

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    private func tap(_ app: XCUIApplication, _ id: String, timeout: TimeInterval) {
        let e = element(app, id)
        XCTAssertTrue(e.waitForExistence(timeout: timeout), diagnose(app, id))
        e.tap()
    }

    /// Replace a text field's contents: delete what the field holds (a value
    /// equal to its placeholder is an empty field), then type `text`.
    private func clearAndType(_ field: XCUIElement, _ text: String) {
        field.tap()
        let current = field.value as? String ?? ""
        if !current.isEmpty, current != field.placeholderValue {
            field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: current.count))
        }
        field.typeText(text)
    }

    private func button(_ app: XCUIApplication, labels: [String]) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label IN %@", labels)).firstMatch
    }

    private func signedOut(_ testFlight: XCUIApplication) -> Bool {
        button(testFlight, labels: ["Sign In", "Continue with Apple Account"]).waitForExistence(timeout: 5)
    }

    private func errorText(_ app: XCUIApplication) -> String {
        let e = element(app, Ids.errorMessage)
        return e.exists ? e.label : ""
    }

    private func diagnose(_ app: XCUIApplication, _ id: String) -> String {
        "'\(id)' not found; error element: \(errorText(app)); visible: \(visibleTexts(app))"
    }

    private func visibleTexts(_ app: XCUIApplication) -> [String] {
        app.staticTexts.allElementsBoundByIndex.prefix(40).map(\.label)
    }
}

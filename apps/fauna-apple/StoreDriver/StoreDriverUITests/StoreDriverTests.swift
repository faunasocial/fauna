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
//
// The `store-install` outcomes (docs/features/store-install.md) are absence
// checks: each reaches the screen that would carry a money or in-person-share
// control in a full build, proves it is ON that screen by an id the store flavor
// keeps, then asserts the excised ids are not in the tree. The ids asserted
// absent sit between the `store-absent` markers below, and
// `test_ios_store_driver_pins.py` holds each to two facts: the store flavor
// excises it (`#if !FAUNA_EXCISE_*` in `UiIds.swift`), and an iOS view paints it
// in a full build — so no absence here is of an id nothing ever drew. Where a
// full build paints the control only on some data (a tipped post, a linked Nostr
// account, a nest advertising `p2p-share`) the device read is a lower bound;
// each such method prints its reading as a `STORE-DRIVER reading=` line, and the
// strict proof stays the build's own (dynamic-features.md § What "completely
// compiled away" means).

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
    /// The compose audience picker's "Public" and "Sell this post…" answers, read
    /// by the host from `i18n/strings/en.yaml` (`feed.post.gate_public` /
    /// `gate_sell`): a menu item carries no identifier, so outcome 3 reads it by
    /// its label, and the host keeps that label in step with the app's string.
    static var gatePublicLabel: String { trimmed("FAUNA_STORE_GATE_PUBLIC_LABEL") }
    static var gateSellLabel: String { trimmed("FAUNA_STORE_GATE_SELL_LABEL") }
    /// The author the buyer halves of outcomes 2 and 3 read: the box's seeding
    /// account, which a FULL build seeded with a priced tier and a sold post
    /// (the provisioning drive's seed). The store build cannot make either, so
    /// the content is the box's, and the driver only reads it.
    static var sellerHandle: String { trimmed("FAUNA_STORE_SELLER_HANDLE") }
    /// How long TestFlight's download and install may take after Install.
    static var installBudget: TimeInterval { TimeInterval(env["FAUNA_STORE_INSTALL_BUDGET_S"] ?? "") ?? 600 }
}

// store-absent:begin — every id in this block is asserted ABSENT from the store
// install; `test_ios_store_driver_pins.py` reads the block. An id the driver
// reads as present belongs outside it.
private enum Excised {
    /// Outcome 1: no payment provider to connect, no code to hand out (own
    /// profile → Tiers) …
    static let providerAndMint = [
        Ids.subscriptionProviderAddButton, Ids.subscriptionProviderSection,
        Ids.subscriptionClaimSection, Ids.subscriptionClaimMintButton,
    ]
    /// … and no code to redeem (Settings → Subscriptions).
    static let redeem = [Ids.subscriptionClaimRedeemInput, Ids.subscriptionClaimRedeemButton]
    /// Outcome 2, the author half: the tier editor's three money fields.
    static let tierMoney = [
        Ids.subscriptionTierFormPriceHint, Ids.subscriptionTierFormAskingPrice,
        Ids.subscriptionTierFormPaymentUrl,
    ]
    /// Outcome 3, the author half: the sale controls under compose.
    static let sell = [Ids.composeSellPrice, Ids.composeSellAskingPrice, Ids.composeSellSubscribersFree]
    /// Outcome 2, the buyer half: another author's priced tier, on their
    /// profile's Tiers tab, shows no price and no payment link.
    static let offerMoney = [
        Ids.subscriptionOfferPrice, Ids.subscriptionOfferPaymentLink, Ids.subscriptionTierPrice,
    ]
    /// Outcome 3, the buyer half: a post sold elsewhere shows no price, payment
    /// link or buy button beside its badge.
    static let soldPostMoney = [Ids.gatedPostPrice, Ids.gatedPostBuyButton, Ids.gatedPostPaymentLink]
    /// Outcome 4: a post's tip surface …
    static let tips = [Ids.postTipTotal, Ids.postTipCount, Ids.postTipListButton]
    /// … and the zap-signer section of the Nostr settings.
    static let zapSigner = [Ids.nostrZapSignerAddBtn, Ids.nostrZapSignerPubkeyInput, Ids.nostrZapSignerEmpty]
    /// Outcome 5, the App Store half: the in-person share and the peer-transfer
    /// readings, both on Settings → Folders.
    static let inPersonShare = [
        Ids.offlineShareButton, Ids.offlineReceiveButton, Ids.offlineShareOwnCode,
        Ids.shareTransferList, Ids.shareServeStatus,
    ]
}
// store-absent:end

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
        let app = try launch()
        try signIn(app)
        try waitForMainTabs(app, after: "signing in as \(Input.accountHandle)")
    }

    // MARK: - store-install: the store flavor's absences

    /// `store-install` outcome 1: no payment provider to connect and no code to
    /// hand out or redeem for a paid tier. The provider and mint halves sit on the
    /// own profile's Tiers tab, the redeem half on Settings → Subscriptions.
    func testStoreNoProviderNoCode() throws {
        let app = try openSignedIn()
        try openOwnTiers(app)
        assertAbsent(app, Excised.providerAndMint, on: "the own profile's Tiers tab")

        let settings = try openSettingsPage(key: "subscription-settings", anchors: [Ids.subscriptionMineSection])
        assertAbsent(settings, Excised.redeem, on: "Settings → Subscriptions")
    }

    /// `store-install` outcome 2, the author half: a tier you create has no price,
    /// price hint or payment link to fill in. The form is opened and cancelled —
    /// nothing is saved.
    func testStoreTierEditorHasNoPrice() throws {
        let app = try openSignedIn()
        try openOwnTiers(app)
        tap(app, Ids.subscriptionTierCreateButton, timeout: 30)
        XCTAssertTrue(element(app, Ids.subscriptionTierForm).waitForExistence(timeout: 30),
                      diagnose(app, Ids.subscriptionTierForm))
        XCTAssertTrue(element(app, Ids.subscriptionTierFormSave).exists, diagnose(app, Ids.subscriptionTierFormSave))
        assertAbsent(app, Excised.tierMoney, on: "the tier editor")
        tap(app, Ids.subscriptionTierFormCancel, timeout: 10)
    }

    /// `store-install` outcome 3, the author half: writing a post never offers to
    /// sell it — the audience picker has no "Sell this post…" answer and no sale
    /// control is painted. The picker is opened and closed on "Public"; nothing
    /// is posted.
    func testStoreComposeNeverOffersASale() throws {
        let publicLabel = Input.gatePublicLabel
        let sellLabel = Input.gateSellLabel
        guard !publicLabel.isEmpty, !sellLabel.isEmpty else {
            XCTFail("the host passed no compose-gate labels (FAUNA_STORE_GATE_PUBLIC_LABEL, FAUNA_STORE_GATE_SELL_LABEL)")
            return
        }
        let app = try openSignedIn()
        try selectTab(app, index: TabIndex.feed)
        let picker = element(app, Ids.composeGateTierSelect)
        XCTAssertTrue(picker.waitForExistence(timeout: 60), diagnose(app, Ids.composeGateTierSelect))
        XCTAssertTrue(element(app, Ids.composeTextField).exists, diagnose(app, Ids.composeTextField))
        assertAbsent(app, Excised.sell, on: "compose")

        picker.tap()
        let publicItem = labelled(app, publicLabel)
        XCTAssertTrue(publicItem.waitForExistence(timeout: 15),
                      "the audience picker opened no menu with a '\(publicLabel)' answer; visible: \(visibleTexts(app))")
        XCTAssertFalse(labelled(app, sellLabel).exists,
                       "the store install's audience picker offers '\(sellLabel)' — the sale is not excised")
        print("STORE-DRIVER absent on the audience picker: \(sellLabel)")
        publicItem.tap()
    }

    /// `store-install` outcome 2, the buyer half: a tier that carries a price
    /// elsewhere shows as an ordinary tier to subscribe to, with no price. The
    /// seller's profile is opened through Find User, its Tiers tab lists the
    /// seeded priced tier (the row is the proof the screen holds it), and none of
    /// the price or payment-link ids is in the tree. Nothing is subscribed to.
    func testStorePricedTierShowsNoPrice() throws {
        let app = try openSignedIn()
        try openSellerProfile(app)
        tap(app, Ids.profileTiersTab, timeout: 30)
        XCTAssertTrue(element(app, Ids.subscriptionOffersSection).waitForExistence(timeout: 60),
                      diagnose(app, Ids.subscriptionOffersSection))
        XCTAssertTrue(element(app, Ids.subscriptionOfferRow).waitForExistence(timeout: 60),
                      "\(Input.sellerHandle) offers no tier — the box holds no priced tier from the full-build seed; "
                      + diagnose(app, Ids.subscriptionOfferRow))
        assertAbsent(app, Excised.offerMoney, on: "\(Input.sellerHandle)'s Tiers tab")
    }

    /// `store-install` outcome 3, the buyer half: a post sold elsewhere shows as a
    /// gated post with its badge and no price, payment link or buy button. The
    /// seller's Posts tab paints the seeded sold post with its badge (the proof),
    /// and none of the teaser's ids is in the tree. A full build paints the teaser
    /// once the post's offer resolves, so the reading is a lower bound; nothing is
    /// bought.
    func testStoreSoldPostShowsNoPrice() throws {
        let app = try openSignedIn()
        try openSellerProfile(app)
        tap(app, Ids.profilePostsTab, timeout: 30)
        XCTAssertTrue(element(app, Ids.gatedPostBadge).waitForExistence(timeout: 60),
                      "\(Input.sellerHandle)'s Posts tab shows no gated post — the box holds no sold post from the "
                      + "full-build seed; " + diagnose(app, Ids.gatedPostBadge))
        assertAbsent(app, Excised.soldPostMoney, on: "\(Input.sellerHandle)'s Posts tab")
    }

    /// `store-install` outcome 4: a post shows no tip total, tip count or
    /// tippers, and nobody can be named as your zap signer.
    func testStoreNoTipsNoZapSigner() throws {
        let app = try openSignedIn()
        try selectTab(app, index: TabIndex.feed)
        XCTAssertTrue(element(app, Ids.composeGateTierSelect).waitForExistence(timeout: 60),
                      diagnose(app, Ids.composeGateTierSelect))
        if element(app, Ids.postCard).waitForExistence(timeout: 30) {
            print("STORE-DRIVER reading=feed_has_post")
            assertAbsent(app, Excised.tips, on: "the feed's post cards")
        } else {
            // A full build paints the tip surface only on a tipped post, so an
            // empty feed reads nothing either way; the zap-signer half still runs.
            print("STORE-DRIVER reading=feed_empty")
        }

        let settings = try openSettingsPage(key: "nostr", anchors: [Ids.nostrUnlinkButton, Ids.nostrLinkMode])
        // A full build paints the zap-signer section for a linked account only.
        let linked = element(settings, Ids.nostrUnlinkButton).exists
        print("STORE-DRIVER reading=nostr_\(linked ? "linked" : "unlinked")")
        assertAbsent(settings, Excised.zapSigner, on: "Settings → Nostr")
    }

    /// `store-install` outcome 5, the App Store half: sharing a folder in person
    /// with someone else's device is left out — Settings → Folders paints neither
    /// the offline-share panel nor the peer-transfer readings (a full build paints
    /// them only where the nest advertises `p2p-share`).
    func testStoreLeavesOutTheInPersonShare() throws {
        _ = try openSignedIn()
        let settings = try openSettingsPage(key: "folders", anchors: [Ids.folderAddButton])
        assertAbsent(settings, Excised.inPersonShare, on: "Settings → Folders")
    }

    // MARK: - navigation, from outside

    /// `MainTabView`'s tab order. A SwiftUI tab item takes no accessibility
    /// identifier, so the bar's items are addressed by position.
    private enum TabIndex {
        static let feed = 1
        static let contacts = 2
        static let more = 3
    }

    /// Launch the installed app afresh.
    private func launch() throws -> XCUIApplication {
        let app = XCUIApplication(bundleIdentifier: faunaBundleID)
        app.terminate()
        app.activate()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 60),
                      "the installed app never came to the foreground")
        return app
    }

    /// Launch the installed app afresh and reach the main tab view, signing in
    /// first when the install opens on onboarding — so every outcome method
    /// stands alone, in whatever order the host runs them.
    private func openSignedIn() throws -> XCUIApplication {
        let app = try launch()
        let tabView = element(app, Ids.mainTabView)
        let onboarding = element(app, Ids.importIdentityButton)
        let deadline = Date().addingTimeInterval(60)
        while Date() < deadline {  // deadline-ok: waits for whichever root the launch shows, a state
            if tabView.exists || app.tabBars.firstMatch.exists { return app }
            if onboarding.exists { break }
            _ = tabView.waitForExistence(timeout: 2)
        }
        try signIn(app)
        try waitForMainTabs(app, after: "signing in as \(Input.accountHandle)")
        return app
    }

    private func selectTab(_ app: XCUIApplication, index: Int) throws {
        let bar = app.tabBars.firstMatch
        XCTAssertTrue(bar.waitForExistence(timeout: 30), "no tab bar on screen; visible: \(visibleTexts(app))")
        let item = bar.buttons.element(boundBy: index)
        XCTAssertTrue(item.waitForExistence(timeout: 10),
                      "the tab bar has no item \(index); items: \(bar.buttons.allElementsBoundByIndex.map(\.label))")
        item.tap()
    }

    /// A row of the More tab's root list (`<view>-tab`), leaving any page a
    /// previous visit left open first.
    private func openMoreRow(_ app: XCUIApplication, _ rowId: String) throws {
        try selectTab(app, index: TabIndex.more)
        let back = element(app, Ids.moreBackButton)
        if back.waitForExistence(timeout: 3) { back.tap() }
        XCTAssertTrue(element(app, Ids.moreView).waitForExistence(timeout: 15), diagnose(app, Ids.moreView))
        tap(app, rowId, timeout: 15)
    }

    /// The own profile's Tiers tab, its tier list loaded.
    private func openOwnTiers(_ app: XCUIApplication) throws {
        try openMoreRow(app, Ids.profileTab)
        tap(app, Ids.profileTiersTab, timeout: 30)
        XCTAssertTrue(element(app, Ids.subscriptionTiersSection).waitForExistence(timeout: 60),
                      diagnose(app, Ids.subscriptionTiersSection))
        XCTAssertTrue(element(app, Ids.subscriptionTierCreateButton).waitForExistence(timeout: 30),
                      diagnose(app, Ids.subscriptionTierCreateButton))
    }

    /// The seller's profile, opened as a reader opens anyone's: Contacts → Find
    /// User by handle → Open profile (`contacts.md` § Layout & flow region 3).
    private func openSellerProfile(_ app: XCUIApplication) throws {
        let handle = Input.sellerHandle
        guard !handle.isEmpty else {
            XCTFail("the host passed no seller handle (FAUNA_STORE_SELLER_HANDLE)")
            return
        }
        try selectTab(app, index: TabIndex.contacts)
        let field = element(app, Ids.contactActorIdField)
        XCTAssertTrue(field.waitForExistence(timeout: 30), diagnose(app, Ids.contactActorIdField))
        clearAndType(field, handle)
        tap(app, Ids.contactActorIdLookup, timeout: 10)
        XCTAssertTrue(element(app, Ids.contactActorIdResult).waitForExistence(timeout: 60),
                      "Find User never resolved \(handle); " + diagnose(app, Ids.contactActorIdResult))
        tap(app, Ids.contactActorIdOpenProfileButton, timeout: 10)
        XCTAssertTrue(element(app, Ids.profilePostsTab).waitForExistence(timeout: 60),
                      diagnose(app, Ids.profilePostsTab))
    }

    /// A Settings sub-page by its rail row (`settings-nav-row[<key>]`, the key
    /// `SettingsPage.pageKey` gives), reached from a fresh launch and on screen
    /// once any of `anchors` is.
    @discardableResult
    private func openSettingsPage(key: String, anchors: [String]) throws -> XCUIApplication {
        let app = try launch()
        try waitForMainTabs(app, after: "relaunching")
        try openMoreRow(app, Ids.settingsTab)
        let rowId = "\(Ids.settingsNavRow)[\(key)]"
        let row = element(app, rowId)
        // A SwiftUI list builds its rows lazily: a row below the fold is not in
        // the tree until the list is scrolled to it.
        for _ in 0..<10 where !(row.exists && row.isHittable) { app.swipeUp() }
        XCTAssertTrue(row.exists && row.isHittable, diagnose(app, rowId))
        row.tap()
        let found = anchors.map { element(app, $0) }
        let deadline = Date().addingTimeInterval(60)
        while Date() < deadline {  // deadline-ok: waits for the page's own anchor, a state
            if found.contains(where: \.exists) { return app }
            _ = found[0].waitForExistence(timeout: 2)
        }
        XCTFail("Settings → \(key) never showed any of \(anchors); error element: \(errorText(app)); "
                + "visible: \(visibleTexts(app))")
        return app
    }

    /// Assert none of `ids` is in the tree, the screen's anchor already found.
    private func assertAbsent(_ app: XCUIApplication, _ ids: [String], on screen: String) {
        let present = ids.filter { element(app, $0).exists }
        XCTAssertTrue(present.isEmpty,
                      "the store install paints \(present) on \(screen) — the store flavor's excision did not hold")
        print("STORE-DRIVER absent on \(screen): \(ids.joined(separator: ","))")
    }

    private func labelled(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
    }

    /// Sign in through the app's own onboarding UI, as the App Store's reviewer
    /// does from the review notes: import the identity, type the handle,
    /// continue.
    private func signIn(_ app: XCUIApplication) throws {
        let handle = Input.accountHandle
        let secret = Input.accountSecret
        guard !handle.isEmpty, secret.count == 64 else {
            throw XCTSkip("state=no_account the host passed no reviewer account (FAUNA_STORE_ACCOUNT_HANDLE and a 64-hex FAUNA_STORE_ACCOUNT_SECRET) — the sign-in leg imports the apple-reviewer account on test.fauna.build")
        }

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
    }

    /// Signed in: the main tab view is on screen (the android witness waits
    /// for `feed-tab`). On iOS the tab bar's `{page}-tab` ids are registered
    /// with the in-process registry only (a SwiftUI tab item takes no
    /// accessibility identifier; `test_ios_store_driver_pins.py` refuses a
    /// driver that reads one), so the release tree is read at the container the
    /// view does identify, `main-tab-view`, or at the tab bar it hosts —
    /// whichever the first device run finds, it reports. (The More list's
    /// `<view>-tab` rows are buttons that do carry their id.)
    private func waitForMainTabs(_ app: XCUIApplication, after step: String) throws {
        let tabView = element(app, Ids.mainTabView)
        let tabBar = app.tabBars.firstMatch
        let deadline = Date().addingTimeInterval(180)
        while Date() < deadline {  // deadline-ok: waits on the signed-in container's presence, a state
            if tabView.exists { print("STORE-DRIVER signed_in=main-tab-view"); return }
            if tabBar.exists { print("STORE-DRIVER signed_in=tab-bar"); return }
            _ = tabView.waitForExistence(timeout: 2)
        }
        XCTFail("the store-installed app never reached the main tab view after \(step); "
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

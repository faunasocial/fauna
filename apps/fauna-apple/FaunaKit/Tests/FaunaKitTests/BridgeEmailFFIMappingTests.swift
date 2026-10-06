import Testing
import Foundation
@testable import FaunaKit

// Pure, nest-free tests for the WS-RPC migration's type plumbing: the
// `FfiCborValue` ⇄ Swift conversions, the `Ffi*` → FaunaKit-struct bridge
// mappings, and the email-filter dialog → typed `Ffi*` composition. The
// round-trip wire conformance lives nest-side
// (`conformance_{bridges_ui,email_filters}.rs`); these guard the Swift seam.

// MARK: - CBOR value conversion

@Test func ffiCborValueDetectsBoolBeforeInt() {
    // SwiftUI toggles hand us a native Bool — it must not collapse to Integer.
    #expect(ffiCborValue(from: true) == .bool(v: true))
    #expect(ffiCborValue(from: false) == .bool(v: false))
}

@Test func ffiCborValueDetectsScalars() {
    #expect(ffiCborValue(from: 42) == .integer(v: 42))
    #expect(ffiCborValue(from: "hi") == .text(v: "hi"))
    #expect(ffiCborValue(from: 1.5) == .float(v: 1.5))
}

@Test func ffiCborMapWrapsDictEntries() {
    let v = ffiCborMap(from: ["write_through": true])
    guard case let .map(entries) = v else { Issue.record("expected a map"); return }
    #expect(entries.count == 1)
    #expect(entries.first?.key == "write_through")
    #expect(entries.first?.value == .bool(v: true))
}

@Test func anyCodableFromCborScalars() {
    #expect(AnyCodable(ffiCbor: .bool(v: true)).boolValue == true)
    #expect(AnyCodable(ffiCbor: .integer(v: 7)).intValue == 7)
    #expect(AnyCodable(ffiCbor: .text(v: "x")).stringValue == "x")
}

// MARK: - Bridge struct mapping

@Test func bridgeInfoFromFfiMapsNestedSettingsAndModes() {
    let ffi = FfiBridgeStatus(
        id: "bluesky", name: "Bluesky", available: true, linked: true,
        identity: FfiBridgeIdentity(label: "Handle", value: "did:plc:abc",
                                    display: "alice.bsky.social"),
        mode: "personal",
        settings: [FfiBridgeSetting(key: "crosspost", label: "Crosspost",
                                    settingType: "bool", value: .bool(v: true),
                                    options: nil)],
        supportsFollows: true,
        linkModes: [FfiBridgeLinkMode(mode: "personal", label: "Personal",
                                      clientAction: nil, platform: nil,
                                      fields: [FfiBridgeLinkField(key: "handle",
                                                                  label: "Handle",
                                                                  fieldType: "text",
                                                                  placeholder: nil)])],
        error: nil)
    let info = BridgeInfo(ffi: ffi)
    #expect(info.id == "bluesky")
    #expect(info.name == "Bluesky")
    #expect(info.supportsFollows)
    #expect(info.identity?.display == "alice.bsky.social")
    #expect(info.settings.count == 1)
    #expect(info.settings[0].type == "bool")
    #expect(info.settings[0].value.boolValue == true)
    #expect(info.linkModes?.first?.fields.first?.key == "handle")
}

@Test func bridgeFeedSubscriptionMapsInt64IdToString() {
    let sub = BridgeFeedSubscription(ffi: FfiFeedSubscription(
        id: 99, bridge: "rss", feedUri: "https://x/feed", name: "X",
        createdAt: 1_700_000_000))
    #expect(sub.id == "99")
    #expect(sub.feedUri == "https://x/feed")
    #expect(sub.createdAt == 1_700_000_000)
}

@Test func bridgeFollowMapsOptionalCreatedAt() {
    let some = BridgeFollow(ffi: FfiBridgeFollow(id: "did:plc:x", petname: "Al",
                                                 createdAt: 123, extra: nil))
    #expect(some.id == "did:plc:x")
    #expect(some.createdAt == 123)
    let none = BridgeFollow(ffi: FfiBridgeFollow(id: "y", petname: nil,
                                                 createdAt: nil, extra: nil))
    #expect(none.createdAt == nil)
}

@Test func bridgeLinkResponseMapsRedirect() {
    let r = BridgeLinkResponse(ffi: FfiLinkReply(linked: false, identity: nil,
                                                 redirectUrl: "https://oauth"))
    #expect(!r.linked)
    #expect(r.redirectUrl == "https://oauth")
}

// MARK: - Email filter dialog → typed composition

@Test func emailFilterRuleFromValidKinds() throws {
    #expect(try encodeEmailFilterRule(kind: "SenderIs", value: "a@b.c") == .senderIs(address: "a@b.c"))
    #expect(try encodeEmailFilterRule(kind: "SenderDomain", value: "b.c") == .senderDomain(domain: "b.c"))
    #expect(try encodeEmailFilterRule(kind: "SubjectContains", value: "hi") == .subjectContains(text: "hi"))
    #expect(try encodeEmailFilterRule(kind: "BodyContains", value: "yo") == .bodyContains(text: "yo"))
}

@Test func emailFilterRuleFromLegacyBrokenKindThrows() {
    // The old iOS picker offered "sender"/"subject"/"body" — invalid wire
    // kinds. The shared encoder must reject them rather than emit a bad rule.
    #expect(throws: (any Error).self) {
        _ = try encodeEmailFilterRule(kind: "sender", value: "x")
    }
}

@Test func emailFilterActionFromTags() throws {
    func tagInputs(_ kind: String) -> FfiFilterActionInputs {
        FfiFilterActionInputs(kind: kind, rejectReason: "", forwardAddress: "", keepLocalCopy: true)
    }
    #expect(try encodeEmailFilterActionInputs(inputs: tagInputs("Allow")) == .allow)
    #expect(try encodeEmailFilterActionInputs(inputs: tagInputs("Discard")) == .discard)
    // Empty rejectReason → the shared encoder fills the canonical default.
    guard case let .reject(reason) = try encodeEmailFilterActionInputs(inputs: tagInputs("Reject")) else {
        Issue.record("expected a reject action"); return
    }
    #expect(!reason.isEmpty)
}

@Test func emailFilterActionInputsEncodeAndDescribeRoundTrip() throws {
    func inputs(_ kind: String, forward: String = "", keep: Bool = true) -> FfiFilterActionInputs {
        FfiFilterActionInputs(kind: kind, rejectReason: "", forwardAddress: forward, keepLocalCopy: keep)
    }
    #expect(try encodeEmailFilterActionInputs(inputs: inputs("Allow")) == .allow)
    #expect(try encodeEmailFilterActionInputs(inputs: inputs("Discard")) == .discard)
    // Keep-a-copy checked (the default) is `redirect: false`; unchecked is `redirect: true`.
    #expect(try encodeEmailFilterActionInputs(inputs: inputs("Forward", forward: "a@b.c"))
            == .forward(address: "a@b.c", redirect: false))
    let redirected = try encodeEmailFilterActionInputs(inputs: inputs("Forward", forward: "a@b.c", keep: false))
    #expect(redirected == .forward(address: "a@b.c", redirect: true))
    // The reverse recovers the destination and copy mode an edit re-opens with.
    let described = describeEmailFilterActionInputs(action: redirected)
    #expect(described?.kind == "Forward")
    #expect(described?.forwardAddress == "a@b.c")
    #expect(described?.keepLocalCopy == false)
}

@Test func emailFilterForwardWithoutADestinationThrows() {
    #expect(throws: (any Error).self) {
        _ = try encodeEmailFilterActionInputs(
            inputs: FfiFilterActionInputs(kind: "Forward", rejectReason: "", forwardAddress: "", keepLocalCopy: true))
    }
}

@Test func emailFilterIsEditableForCoversForwardOnlyWhenOffered() {
    let rules: [FfiEmailFilterRule] = [.senderIs(address: "a@b.c")]
    let forward = FfiEmailFilterAction.forward(address: "x@y.z", redirect: false)
    #expect(emailFilterIsEditableFor(rules: rules, action: forward,
                                     actionKinds: EmailFilterOptions.actions.map(\.tag)))
    #expect(!emailFilterIsEditableFor(rules: rules, action: forward,
                                      actionKinds: ["Allow", "Discard", "Reject"]))
}

@Test func emailFilterActionLabelsCoverAllVariants() {
    #expect(FfiEmailFilterAction.allow.label == "Allow")
    #expect(FfiEmailFilterAction.discard.label == "Discard")
    #expect(FfiEmailFilterAction.reject(reason: "x").label == "Reject")
    #expect(FfiEmailFilterAction.fileInto(mailbox: "Archive").label == "File")
    #expect(FfiEmailFilterAction.forward(address: "a@b", redirect: false).label == "Forward")
}

@Test func emailFilterPickerOptionsAreUnifiedAndValid() throws {
    // Every rule-kind tag the (now shared) picker offers must compose a real
    // rule via the shared encoder — the guard against iOS/macOS picker drift recurring.
    for opt in EmailFilterOptions.ruleKinds {
        _ = try encodeEmailFilterRule(kind: opt.tag, value: "v")
    }
    #expect(EmailFilterOptions.actions.contains { $0.tag == "Allow" })
    #expect(EmailFilterOptions.actions.contains { $0.tag == "Forward" })
}

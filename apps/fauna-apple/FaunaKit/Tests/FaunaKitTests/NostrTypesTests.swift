import Testing
import Foundation
@testable import FaunaKit

// The Nostr control plane rides the unified `fauna.bridges.*` kinds
// (`bridge_id:"nostr"`); these cover the `BridgeInfo`/`BridgeFollow` →
// Nostr-page view-type mappers + the settings projection.
@Suite("NostrTypes")
struct NostrTypesTests {
    private func bridgeInfo(_ json: String) throws -> BridgeInfo {
        try JSONDecoder().decode(BridgeInfo.self, from: json.data(using: .utf8)!)
    }

    @Test func mapLinkedStatus() throws {
        let info = try bridgeInfo("""
        {"id":"nostr","name":"Nostr","available":true,"linked":true,
         "identity":{"label":"npub","value":"npub1abc","display":"npub1abc…x"},
         "mode":"generate","supports_follows":true,
         "settings":[
           {"key":"expose_content","label":"","type":"bool","value":true,"options":null},
           {"key":"auto_publish","label":"","type":"bool","value":false,"options":null},
           {"key":"publish_replies","label":"","type":"bool","value":true,"options":null},
           {"key":"publish_reactions","label":"","type":"bool","value":false,"options":null},
           {"key":"inbound_to_feed","label":"","type":"bool","value":true,"options":null},
           {"key":"relay_list","label":"","type":"string","value":"wss://relay.example","options":null}
         ],"link_modes":null}
        """)
        let status = NostrStatus(bridge: info)
        #expect(status.linked == true)
        #expect(status.available == true)
        #expect(status.pubkey == "npub1abc")  // npub from identity.value
        #expect(status.mode == "generate")
        // Keyed by wire key, not five named properties — and only the *bool*
        // settings land in `flags` (`relay_list` is a string).
        #expect(status.flags["expose_content"] == true)
        #expect(status.flags["auto_publish"] == false)
        #expect(status.flags["publish_replies"] == true)
        #expect(status.flags["publish_reactions"] == false)
        #expect(status.flags["inbound_to_feed"] == true)
        #expect(status.flags["relay_list"] == nil)
        #expect(status.relayList == "wss://relay.example")
    }

    /// A reported key wins over the catalog default; an unreported one falls
    /// back to it — the read half of the catalog contract the view renders on.
    @Test func flagFallsBackToCatalogDefault() throws {
        let info = try bridgeInfo("""
        {"id":"nostr","name":"Nostr","available":true,"linked":true,
         "identity":null,"mode":null,"supports_follows":true,
         "settings":[
           {"key":"expose_content","label":"","type":"bool","value":true,"options":null}
         ],"link_modes":null}
        """)
        let status = NostrStatus(bridge: info)
        #expect(status.flag("expose_content", default: false) == true)
        // Never reported: the nest's own default (what the catalog carries),
        // not a client-side `false`.
        #expect(status.flag("publish_replies", default: true) == true)
        #expect(status.flag("auto_publish", default: false) == false)
    }

    /// The view renders straight off the shared catalog — so every row it
    /// hands over must be one this page can actually drive: a `ui.yaml`
    /// `nostr-*` element id, and a key the bridge's settings speak.
    @Test func catalogRowsMatchThePageContract() {
        let options = nostrContentToggleOptions()
        #expect(options.count == 5)
        #expect(options.map(\.key) == [
            "expose_content", "auto_publish", "publish_replies",
            "publish_reactions", "inbound_to_feed",
        ])
        #expect(options.map(\.uiId) == [
            "nostr-expose-content", "nostr-auto-publish", "nostr-publish-replies",
            "nostr-publish-reactions", "nostr-inbound-to-feed",
        ])
        // Every label resolves through the app's own i18n table — a key the
        // generated `L` does not carry would render as the raw key.
        for option in options {
            #expect(renderLocalizedText(option.label) != option.label.key)
            if let subtitle = option.subtitle {
                #expect(renderLocalizedText(subtitle) != subtitle.key)
            }
        }
    }

    @Test func mapUnlinkedUnavailableStatus() throws {
        let info = try bridgeInfo("""
        {"id":"nostr","name":"Nostr","available":false,"linked":false,
         "identity":null,"mode":null,"supports_follows":true,
         "settings":[],"link_modes":null}
        """)
        let status = NostrStatus(bridge: info)
        #expect(status.linked == false)
        #expect(status.available == false)
        #expect(status.pubkey == nil)
        #expect(status.flags.isEmpty)
    }

    @Test func mapFollow() throws {
        let follow = try JSONDecoder().decode(BridgeFollow.self, from: """
        {"id":"npub1xyz","petname":"alice","created_at":1700000000}
        """.data(using: .utf8)!)
        let mapped = NostrFollow(bridge: follow)
        #expect(mapped.pubkey == "npub1xyz")
        #expect(mapped.petname == "alice")
        #expect(mapped.id == "npub1xyz")
        #expect(mapped.createdAt == 1700000000)
    }

    @Test func settingsToBridgeMapOnlySetKeys() throws {
        let dict = NostrSettings(flags: ["expose_content": true, "auto_publish": false])
            .asBridgeSettings
        #expect(dict["expose_content"] as? Bool == true)
        #expect(dict["auto_publish"] as? Bool == false)
        #expect(dict["relay_list"] == nil)
        #expect(dict.count == 2)  // only the keys the caller set are sent
    }

    // MARK: - Relay list (JSON array <-> [String])

    @Test func parseRelayListDecodesJsonArray() {
        let relays = NostrVM.parseRelayList(#"["wss://relay.damus.io","wss://nos.lol"]"#)
        #expect(relays == ["wss://relay.damus.io", "wss://nos.lol"])
    }

    @Test func parseRelayListEmptyOrNilYieldsEmpty() {
        // The nest stores `None` -> the bridge wire surfaces `""` (unwrap_or_default).
        #expect(NostrVM.parseRelayList(nil).isEmpty)
        #expect(NostrVM.parseRelayList("").isEmpty)
        #expect(NostrVM.parseRelayList("   ").isEmpty)
        #expect(NostrVM.parseRelayList("[]").isEmpty)
        #expect(NostrVM.parseRelayList("not json").isEmpty)  // tolerate garbage
    }

    @Test func encodeRelayListRoundTripsAndKeepsSlashes() {
        let list = ["wss://relay.damus.io", "wss://nos.lol"]
        let json = NostrVM.encodeRelayList(list)
        #expect(json.contains("wss://relay.damus.io"))  // slashes not escaped
        #expect(NostrVM.parseRelayList(json) == list)    // round-trips
        #expect(NostrVM.encodeRelayList([]) == "[]")
    }

    @Test func linkResponseMapsNpub() throws {
        let reply = try JSONDecoder().decode(BridgeLinkResponse.self, from: """
        {"linked":true,"identity":{"label":"npub","value":"npub1new","display":"npub1new…y"},"redirect_url":null}
        """.data(using: .utf8)!)
        let mapped = NostrLinkResponse(bridge: reply, mode: "generate")
        #expect(mapped.pubkey == "npub1new")
        #expect(mapped.mode == "generate")
    }

    // MARK: - addRelay validation (shared `relayUrlError` FFI)

    @Test @MainActor func addRelayRejectsNonWssScheme() async {
        let vm = NostrVM()
        vm.relayInput = "https://relay.example"
        await vm.addRelay()
        #expect(vm.errorMessage == L.nostr.relays.invalidUrl)
        #expect(vm.relays.isEmpty)
    }

    @Test @MainActor func addRelayRejectsPrivateAddress() async {
        let vm = NostrVM()
        vm.relayInput = "wss://192.168.1.10:7777"
        await vm.addRelay()
        #expect(vm.errorMessage == L.nostr.relays.privateAddress)
        #expect(vm.relays.isEmpty)
    }

    @Test @MainActor func addRelayAcceptsWssScheme() async {
        let vm = NostrVM()
        vm.relayInput = "wss://relay.example"
        await vm.addRelay()
        // No API configured, so the write itself no-ops (`api` is nil) — this
        // only proves the scheme check passed, not that the relay persisted.
        #expect(vm.errorMessage == nil)
    }
}

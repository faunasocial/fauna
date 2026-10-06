import Foundation

// Nostr control-plane view types. These are no longer wire DTOs — the Nostr
// control plane rides the unified `fauna.bridges.*` kinds (`bridge_id:"nostr"`),
// so these are built Swift-side from the generic `BridgeInfo`/`BridgeFollow`
// (see the mapping inits below + `APIClient` § Nostr). The standalone Nostr page
// (`NostrSettingsView`) binds them — Nostr keeps its own dedicated page, the same
// treatment as mail, NOT folded into the unified Bridges page (ratified 2026-06-13;
// `docs/goal/ui/nostr.md` § Page structure / `bridges.md` § Scope).

public struct NostrStatus {
    /// Whether the Nostr bridge is present in `fauna.bridges.list` at all — a
    /// nest built without the `nostr` cargo feature omits it entirely, so this
    /// is `false` genuinely and permanently. Distinct from `available` (the
    /// S8.9 nsec-deposit bootstrap gate: false on a box with zero deposits so
    /// far, but the bridge itself exists) — conflating the two hid the link
    /// form on every fresh box (`nostr.md` § The bridging gate).
    public let registered: Bool
    public let linked: Bool
    /// S8.9 nsec-deposit gate (`NostrProvider::available` /
    /// `any_nsec_deposited()`): false until the FIRST nsec is ever deposited
    /// on this box. Linking itself must stay reachable even when this is
    /// false — it's what bootstraps the deposit — so only `registered` gates
    /// the "unavailable" notice; `available` gates nothing in this view today.
    public let available: Bool
    public let pubkey: String?  // npub (`identity.value`); nil when unlinked
    public let mode: String?
    /// Every boolean setting the bridge reports, keyed by its wire key — NOT
    /// five named properties. The five content-publishing flags' `(key, ui id,
    /// default, title, subtitle)` rows are owned by shared Rust
    /// (`fauna_ffi::bridges::nostr_content_toggle_options`, `nostr.md`
    /// § Where logic lives → *The content-toggle catalog*), so naming them here
    /// would be a sixth copy of that table baked into a *type's shape* — the
    /// form the cross-language duplicate-table scanner cannot see. A key the
    /// bridge does not report is simply absent; the caller falls back to the
    /// catalog's `defaultOn` (the nest's own default), never a client-side
    /// guess. Same shape android reads through its `boolSetting` lookup.
    public let flags: [String: Bool]
    public let relayList: String?

    public init(registered: Bool, linked: Bool, available: Bool, pubkey: String? = nil, mode: String? = nil,
                flags: [String: Bool] = [:], relayList: String? = nil) {
        self.registered = registered
        self.linked = linked
        self.available = available
        self.pubkey = pubkey
        self.mode = mode
        self.flags = flags
        self.relayList = relayList
    }

    /// The bridge's value for `key`, or `defaultOn` when it reports none —
    /// the read half of the catalog contract (apple's twin of android's
    /// `boolSetting(settings, key, default)` and shared Rust's
    /// `fauna_client_bridges::bool_setting`).
    public func flag(_ key: String, default defaultOn: Bool) -> Bool {
        flags[key] ?? defaultOn
    }

    /// Map the Nostr bridge's `fauna.bridges.list` entry into the page view type
    /// (the entry was found, so `registered` is always true here — the caller
    /// synthesizes the not-found case separately). The content flags +
    /// `relay_list` ride `settings[]` keyed by `key`; npub rides
    /// `identity.value`. (`pubkey_hex` is not carried on the generic bridge
    /// wire — provider-leak avoided; it was unused in the UI.)
    public init(bridge: BridgeInfo) {
        // Every boolean setting, keyed — not a five-arm list of the content
        // flags. *Which* keys matter is the shared catalog's answer, asked at
        // render time; this init only carries what the bridge reported.
        var flags: [String: Bool] = [:]
        for setting in bridge.settings {
            if let b = setting.value.boolValue { flags[setting.key] = b }
        }
        self.init(
            registered: true,
            linked: bridge.linked,
            available: bridge.available,
            pubkey: bridge.identity?.value,
            mode: bridge.mode,
            flags: flags,
            relayList: bridge.settings.first { $0.key == "relay_list" }?.value.stringValue)
    }
}

public struct NostrFollow: Identifiable {
    public let pubkey: String  // npub
    public let petname: String?
    public let createdAt: Int

    public var id: String { pubkey }

    public init(pubkey: String, petname: String?, createdAt: Int) {
        self.pubkey = pubkey
        self.petname = petname
        self.createdAt = createdAt
    }

    /// Map a `fauna.bridges.list_follows` row (`id`=npub). apple's generic
    /// `BridgeFollow` drops `extra.relay_hints` (unused in the Nostr UI).
    public init(bridge: BridgeFollow) {
        self.init(pubkey: bridge.id, petname: bridge.petname, createdAt: bridge.createdAt ?? 0)
    }
}

public struct NostrSettings {
    public var relayList: String?
    /// The boolean settings to write, keyed by wire key — the write twin of
    /// [`NostrStatus.flags`]. The caller passes the key the shared catalog
    /// handed it; nothing here re-spells the five content keys.
    public var flags: [String: Bool]

    public init(relayList: String? = nil, flags: [String: Bool] = [:]) {
        self.relayList = relayList
        self.flags = flags
    }

    /// Only-set settings for `fauna.bridges.set_settings`, keyed by the
    /// `NostrProvider` setting keys (NIP-65 republish preserved nest-side).
    public var asBridgeSettings: [String: Any] {
        var dict: [String: Any] = [:]
        if let relayList { dict["relay_list"] = relayList }
        for (key, value) in flags { dict[key] = value }
        return dict
    }
}

public struct NostrLinkResponse {
    public let pubkey: String
    public let mode: String

    /// Map a `fauna.bridges.link` reply (`identity.value`=npub) into the page
    /// type. The caller (NostrVM) discards this and re-fetches via status.
    public init(bridge: BridgeLinkResponse, mode: String) {
        self.pubkey = bridge.identity?.value ?? ""
        self.mode = mode
    }
}

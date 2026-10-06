import Foundation

// Bridges WS-RPC seam: `Ffi*` (UniFFI mirrors of `fauna_protocol::bridges_ui`)
// ⇄ the FaunaKit structs the SwiftUI Bridges/Feed surfaces already bind to.
// The Swift structs are faithful (camelCase + `AnyCodable`) mirrors of the
// `Ffi*` records, so these maps are lossless and the view models keep their
// signatures — the migration is confined to APIClient.swift's call sites.
// (Email filters are *not* mapped to a Swift struct: their old shape had
// drifted from the protocol, so they ride the typed `Ffi*` directly — see
// SettingsTypes.swift.)

// MARK: - CBOR value bridging

/// A dynamic UI value (a bridge setting, a link-form field) → self-describing
/// `FfiCborValue` for the wire. Order matters: a native `Bool` must be caught
/// before `Int`, else a toggle would serialize as `1`/`0`.
func ffiCborValue(from value: Any) -> FfiCborValue {
    switch value {
    case let b as Bool: return .bool(v: b)
    case let i as Int: return .integer(v: Int64(i))
    case let i as Int64: return .integer(v: i)
    case let d as Double: return .float(v: d)
    case let s as String: return .text(v: s)
    case let dict as [String: Any]: return ffiCborMap(from: dict)
    case let arr as [Any]: return .array(items: arr.map(ffiCborValue(from:)))
    default: return .null
    }
}

/// `[String: Any]` (a settings patch, a link-mode field set) → a CBOR map.
func ffiCborMap(from dict: [String: Any]) -> FfiCborValue {
    .map(entries: dict.map { FfiCborEntry(key: $0.key, value: ffiCborValue(from: $0.value)) })
}

extension AnyCodable {
    /// Inbound bridge setting/option values. Bridge settings are scalars in
    /// practice; non-scalar CBOR (`array`/`map`/`bytes`/`null`) has no UI
    /// binding, so it collapses to an empty string rather than crashing.
    init(ffiCbor: FfiCborValue) {
        switch ffiCbor {
        case let .bool(v): self.init(v)
        case let .integer(v): self.init(Int(v))
        case let .float(v): self.init(v)
        case let .text(v): self.init(v)
        case .null, .bytes, .array, .map: self.init("")
        }
    }
}

// MARK: - Record mappings

extension BridgeIdentity {
    init(ffi: FfiBridgeIdentity) {
        self.init(label: ffi.label, value: ffi.value, display: ffi.display)
    }
}

extension BridgeSettingOption {
    init(ffi: FfiBridgeSettingOption) {
        self.init(value: AnyCodable(ffiCbor: ffi.value), label: ffi.label)
    }
}

extension BridgeSetting {
    init(ffi: FfiBridgeSetting) {
        self.init(key: ffi.key, label: ffi.label, type: ffi.settingType,
                  value: AnyCodable(ffiCbor: ffi.value),
                  options: ffi.options.map { $0.map(BridgeSettingOption.init(ffi:)) })
    }
}

extension BridgeLinkField {
    init(ffi: FfiBridgeLinkField) {
        self.init(key: ffi.key, label: ffi.label, type: ffi.fieldType,
                  placeholder: ffi.placeholder)
    }
}

extension BridgeLinkMode {
    init(ffi: FfiBridgeLinkMode) {
        self.init(mode: ffi.mode, label: ffi.label, clientAction: ffi.clientAction,
                  platform: ffi.platform,
                  fields: ffi.fields.map(BridgeLinkField.init(ffi:)))
    }
}

extension BridgeInfo {
    init(ffi: FfiBridgeStatus) {
        self.init(id: ffi.id, name: ffi.name, available: ffi.available,
                  linked: ffi.linked,
                  identity: ffi.identity.map(BridgeIdentity.init(ffi:)),
                  mode: ffi.mode,
                  settings: ffi.settings.map(BridgeSetting.init(ffi:)),
                  supportsFollows: ffi.supportsFollows,
                  linkModes: ffi.linkModes.map { $0.map(BridgeLinkMode.init(ffi:)) },
                  error: ffi.error)
    }
}

extension BridgeLinkResponse {
    init(ffi: FfiLinkReply) {
        self.init(linked: ffi.linked,
                  identity: ffi.identity.map(BridgeIdentity.init(ffi:)),
                  redirectUrl: ffi.redirectUrl)
    }
}

extension BridgeFollow {
    init(ffi: FfiBridgeFollow) {
        self.init(id: ffi.id, petname: ffi.petname,
                  createdAt: ffi.createdAt.map(Int.init))
    }
}

extension BridgeFeedSubscription {
    init(ffi: FfiFeedSubscription) {
        self.init(id: String(ffi.id), bridge: ffi.bridge, feedUri: ffi.feedUri,
                  name: ffi.name, createdAt: Int(ffi.createdAt))
    }
}

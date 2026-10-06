import Foundation

// Bridges the provider registry's `Generated/Providers.swift` types
// (`ProviderMeta`/`Capability`/`FieldType`) to the onboarding FFI's
// plain-data mirrors (`FieldMetaPlain`/`CapabilityPlain`/`FieldTypePlain`,
// `generated/fauna_onboarding_machine.swift`) — two isomorphic enum pairs
// exist because the FFI scaffold can't reach into the generated registry
// types directly.

extension Capability {
    /// Convert to the FFI's isomorphic `CapabilityPlain`.
    var plain: CapabilityPlain {
        switch self {
        case .dns: return .dns
        case .vps: return .vps
        case .registrar: return .registrar
        }
    }
}

extension FieldType {
    /// Convert to the FFI's isomorphic `FieldTypePlain`.
    var plain: FieldTypePlain {
        switch self {
        case .text: return .text
        case .secret: return .secret
        case .select: return .select
        case .hostedAuth: return .hostedAuth
        }
    }
}

extension ProviderMeta {
    /// Filter fields to the VPS-applicable ones and convert to the FFI's
    /// plain-data shape `CredentialsForm.fields` takes. VPS fields are
    /// always those with `kinds.contains(.vps)` — the machine doesn't expose
    /// a `visibleVpsFields()` getter the way it does for DNS (no view-time
    /// gating needed), so this stays client-side glue — shared once here
    /// instead of duplicated per apple app (priority #2; linux does the
    /// same client-side filter in
    /// `apps/fauna-linux/src/views/onboarding/vps_config.rs`).
    public var visibleVpsFields: [FieldMetaPlain] {
        fields
            .filter { $0.kinds.contains(.vps) }
            .map { f in
                FieldMetaPlain(
                    id: f.id,
                    fieldType: f.type.plain,
                    labelKey: f.labelKey,
                    required: f.required,
                    kinds: f.kinds.map { $0.plain }
                )
            }
    }
}

import SwiftUI

/// Credentials form for a selected provider. Used by both DNS and VPS
/// configuration pages. The caller passes `vm.machine.visibleDnsFields()` /
/// `visibleVpsFields()` (already filtered by the machine on the active
/// `kinds:` bitmask), so this view renders the list verbatim with no
/// further filtering.
///
/// `getCred` / `setCred` route through `vm.machine.dnsConfig().creds[id]`
/// and `vm.machine.setDnsCred(...)` respectively (or the VPS equivalents).
public struct CredentialsForm: View {
    /// Already-filtered fields. Use the FFI's plain-data shape so this view
    /// doesn't need to know about the registry's `FieldMeta` (the two have
    /// different `kinds` enum types — `CapabilityPlain` vs. `Capability`).
    public let fields: [FieldMetaPlain]
    public let kind: String
    public let getCred: (String) -> String
    public let setCred: (String, String) -> Void
    /// The `hosted-auth` field's can-begin / state / begin-then-wait calls
    /// (onboarding.md § 4) — every other field type never touches them. Closures
    /// rather than a machine so the `nest_retire` page's own machine drives the
    /// same form (priority #2); the `OnboardingMachine` init below is the
    /// wizard's one-line adapter.
    public let hostedAuthCanBegin: (String) -> Bool
    public let hostedAuthState: (String) -> HostedAuthState
    public let hostedAuthBegin: (String) async -> HostedAuthPrompt?
    public let hostedAuthWait: (String) async -> Void

    public init(
        fields: [FieldMetaPlain],
        kind: String,
        getCred: @escaping (String) -> String,
        setCred: @escaping (String, String) -> Void,
        hostedAuthCanBegin: @escaping (String) -> Bool,
        hostedAuthState: @escaping (String) -> HostedAuthState,
        hostedAuthBegin: @escaping (String) async -> HostedAuthPrompt?,
        hostedAuthWait: @escaping (String) async -> Void
    ) {
        self.fields = fields
        self.kind = kind
        self.getCred = getCred
        self.setCred = setCred
        self.hostedAuthCanBegin = hostedAuthCanBegin
        self.hostedAuthState = hostedAuthState
        self.hostedAuthBegin = hostedAuthBegin
        self.hostedAuthWait = hostedAuthWait
    }

    public init(
        fields: [FieldMetaPlain],
        kind: String,
        getCred: @escaping (String) -> String,
        setCred: @escaping (String, String) -> Void,
        machine: OnboardingMachine,
        form: CredentialForm
    ) {
        self.init(
            fields: fields, kind: kind, getCred: getCred, setCred: setCred,
            hostedAuthCanBegin: { machine.hostedAuthCanBegin(form: form, fieldId: $0) },
            hostedAuthState: { machine.hostedAuthState(form: form, fieldId: $0) },
            hostedAuthBegin: { try? await machine.hostedAuthBegin(form: form, fieldId: $0) },
            hostedAuthWait: { _ = try? await machine.hostedAuthWait(form: form, fieldId: $0) }
        )
    }

    public var body: some View {
        VStack(alignment: .leading) {
            ForEach(fields, id: \.id) { field in
                // One binding, used by both the field and `automationField` (no
                // duplication) — writes route through the same machine setter a
                // keystroke would (mirrors MacHandleEntryView's handleBinding). A
                // bare `.accessibilityIdentifier` is invisible to the in-process
                // driver, so each field also registers via `automationField`.
                let credBinding = Binding(
                    get: { getCred(field.id) },
                    set: { setCred(field.id, $0) }
                )
                let fieldId = "\(kind)-credentials-form-\(field.id)"
                LabeledContent(L.lookup(field.labelKey)) {
                    if field.fieldType == .hostedAuth {
                        hostedAuthButton(field: field, fieldId: fieldId)
                    } else {
                        Group {
                            if field.fieldType == .secret {
                                SecureField("", text: credBinding)
                            } else {
                                TextField("", text: credBinding)
                            }
                        }
                        .accessibilityIdentifier(fieldId)
                        .automationField(fieldId, text: credBinding)
                    }
                }
            }
        }
        .accessibilityIdentifier("\(kind)-credentials-form")
        // Register the container's presence for the in-process driver, and keep
        // the child field ids individually queryable under it (`.contain`, not
        // `.combine`) — a bare container id would otherwise clobber them.
        .automationValue("\(kind)-credentials-form", text: { "" })
        .accessibilityElement(children: .contain)
    }

    /// A `hosted-auth` field (onboarding.md § 4): the bundled provider's device-
    /// authorization flow, entirely machine-owned — this view re-derives no
    /// state, mirroring tui's `hosted_auth_button` one-to-one. No SecureField
    /// fallback once this renders (that degradation stays for an app that
    /// hasn't lifted this yet).
    private func hostedAuthButton(field: FieldMetaPlain, fieldId: String) -> some View {
        Button(hostedAuthLabel(field: field)) {
            beginHostedAuth(field: field)
        }
        .disabled(!hostedAuthCanBegin(field.id))
        .accessibilityIdentifier(fieldId)
        .automationActivate(
            fieldId,
            isEnabled: { hostedAuthCanBegin(field.id) },
            text: { hostedAuthLabel(field: field) }
        ) {
            beginHostedAuth(field: field)
        }
    }

    private func hostedAuthLabel(field: FieldMetaPlain) -> String {
        switch hostedAuthState(field.id) {
        case .idle:
            return L.provisioning.hostedAuth.connect
        case .pending(let userCode, _):
            return L.provisioning.hostedAuth.pending(code: userCode)
        case .connected:
            return L.provisioning.hostedAuth.connected
        case .failed(let message):
            return L.provisioning.hostedAuth.failed(message: message)
        }
    }

    /// Begin → open the provider's hosted sign-in page → wait for the token to
    /// land. Errors are already in `HostedAuthState.failed` (painted as the
    /// button's own label on the next read) — nothing to re-derive here,
    /// exactly tui's `Action::HostedAuth` handler.
    private func beginHostedAuth(field: FieldMetaPlain) {
        Task {
            guard let prompt = await hostedAuthBegin(field.id),
                  let url = URL(string: prompt.verificationUrl)
            else { return }
            OpenURL.open(url)
            await hostedAuthWait(field.id)
        }
    }
}

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
    /// Owns the `hosted-auth` field's begin/wait/state/can-begin calls
    /// (onboarding.md § 4) — every other field type never touches it.
    public let machine: OnboardingMachine
    public let form: CredentialForm

    public init(
        fields: [FieldMetaPlain],
        kind: String,
        getCred: @escaping (String) -> String,
        setCred: @escaping (String, String) -> Void,
        machine: OnboardingMachine,
        form: CredentialForm
    ) {
        self.fields = fields
        self.kind = kind
        self.getCred = getCred
        self.setCred = setCred
        self.machine = machine
        self.form = form
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
        .disabled(!machine.hostedAuthCanBegin(form: form, fieldId: field.id))
        .accessibilityIdentifier(fieldId)
        .automationActivate(
            fieldId,
            isEnabled: { machine.hostedAuthCanBegin(form: form, fieldId: field.id) },
            text: { hostedAuthLabel(field: field) }
        ) {
            beginHostedAuth(field: field)
        }
    }

    private func hostedAuthLabel(field: FieldMetaPlain) -> String {
        switch machine.hostedAuthState(form: form, fieldId: field.id) {
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
            guard let prompt = try? await machine.hostedAuthBegin(form: form, fieldId: field.id),
                  let url = URL(string: prompt.verificationUrl)
            else { return }
            OpenURL.open(url)
            _ = try? await machine.hostedAuthWait(form: form, fieldId: field.id)
        }
    }
}

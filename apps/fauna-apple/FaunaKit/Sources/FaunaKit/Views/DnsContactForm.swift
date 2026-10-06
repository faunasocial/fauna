import SwiftUI

/// The `dns_config` buy-domain extras shared by the macOS + iOS onboarding DNS
/// step: the WHOIS contact form and the per-registrar notes line. Both are pure,
/// machine-agnostic leaf views — the *gating* (`shouldShowContactForm()` /
/// `shouldShowRegistrarNotes()`) stays in the per-app shell, which mounts
/// these only when the shared predicate says so (priority #2: one rendering, two
/// apps; `docs/goal/behavior/onboarding.md` § 4 DNS configuration).

/// WHOIS registration contact form — `dns-contact-form` container with the 9
/// `dns-contact-{first-name,last-name,email,phone,address1,city,state,postal-code,
/// country}-input` fields. Pre-fills from `contact` (the machine's captured
/// `dns_config().contact`, `nil` until `verify_dns()` populates it via
/// `Registrar::fetch_default_contact()`); each edit pushes the whole record back
/// through `onSet`, which the shell wires to `OnboardingMachine.set_contact`.
public struct DnsContactForm: View {
    public let contact: ContactInfo?
    public let onSet: (ContactInfo) -> Void

    public init(contact: ContactInfo?, onSet: @escaping (ContactInfo) -> Void) {
        self.contact = contact
        self.onSet = onSet
    }

    private var current: ContactInfo {
        contact ?? ContactInfo(
            firstName: "", lastName: "", email: "", phone: "",
            address1: "", city: "", state: "", postalCode: "", country: ""
        )
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.onboarding.dnsConfig.contactFormHeading)
                .font(.subheadline.bold())

            contactField("first-name", current.firstName) { update($0) { $0.firstName = $1 } }
            contactField("last-name", current.lastName) { update($0) { $0.lastName = $1 } }
            contactField("email", current.email) { update($0) { $0.email = $1 } }
            contactField("phone", current.phone) { update($0) { $0.phone = $1 } }
            contactField("address1", current.address1) { update($0) { $0.address1 = $1 } }
            contactField("city", current.city) { update($0) { $0.city = $1 } }
            contactField("state", current.state) { update($0) { $0.state = $1 } }
            contactField("postal-code", current.postalCode) { update($0) { $0.postalCode = $1 } }
            contactField("country", current.country) { update($0) { $0.country = $1 } }
        }
        .padding(.vertical, 4)
        .accessibilityIdentifier(Ids.dnsContactForm)
        // Register the container's presence + keep the child field ids queryable
        // under it (`.contain`, not `.combine`) for the in-process driver.
        .automationValue(Ids.dnsContactForm, text: { "" })
        .accessibilityElement(children: .contain)
    }

    private func contactField(_ slug: String, _ value: String, set: @escaping (String) -> Void) -> some View {
        // One binding for the field + `automationField` (a bare
        // `.accessibilityIdentifier` is invisible to the in-process driver).
        let binding = Binding(get: { value }, set: set)
        return TextField("", text: binding)
            .textFieldStyle(.roundedBorder)
            .accessibilityIdentifier("dns-contact-\(slug)-input")
            .automationField("dns-contact-\(slug)-input", text: binding)
            .disableAutocorrection(true)
    }

    /// Read the current contact (empty record if none), apply a per-field
    /// mutation, push the whole record back via `onSet` — mirrors the
    /// target-state rule "user edits push back via set_contact(contact)".
    private func update(_ newValue: String, _ mut: (inout ContactInfo, String) -> Void) {
        var c = current
        mut(&c, newValue)
        onSet(c)
    }
}

/// Per-registrar notes line — `dns-registrar-notes-text`. The note text stays
/// platform-side: the shell looks up the selected provider's `registrar_notes_key`
/// (from the generated provider registry) and hands the localized key here. Shown
/// by the shell only on the buy-domain path for a provider that has the key
/// (Porkbun today, which uses account-level contacts instead of a contact form).
public struct DnsRegistrarNotes: View {
    public let notesKey: String

    public init(notesKey: String) { self.notesKey = notesKey }

    public var body: some View {
        automationText(Ids.dnsRegistrarNotesText, L.lookup(notesKey))
            .font(.callout)
            .foregroundStyle(.secondary)
    }
}

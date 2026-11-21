import Foundation
import Testing
@testable import FaunaKit

// ui/README.md § Offer only what is possible, rules 2 and 6 — the apple leg of
// the onboarding credential buttons tui led (`apps/fauna-tui/src/wizard/
// {dns_config,vps_config,provider_choice,nest_retire,mod}.rs`). Each button is
// painted through `faunaPossible` from the shared machine's reasoned answer,
// never a bool; these pin, over real shared machines and no nest, a blocked
// state (with the reason key the view shows beside the control) and a
// possible state for every button the views read.

private final class NoopObserver: OnboardingObserver, @unchecked Sendable {
    func onChanged() {}
}

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

private func wizard(handle: String? = nil) -> OnboardingMachine {
    let m = OnboardingMachine(observer: NoopObserver())
    if let handle {
        m.beginCreateIdentity()
        m.setCurrentHandle(h: handle)
    }
    return m
}

// MARK: - dns-verify-button / vps-verify-button

@Test func dnsVerifyNamesTheProviderThenTheFieldsThenIsPossible() {
    let m = wizard()
    #expect(reasonKey(m.verifyDnsPossibility()) == "onboarding.dns_config.status_pick_provider")
    m.selectDnsProvider(id: "cloudflare")
    #expect(reasonKey(m.verifyDnsPossibility()) == "provisioning.fill_required_fields")
    m.setDnsCred(fieldId: "api-token", value: "filled")
    #expect(m.verifyDnsPossibility() == .possible)
}

@Test func vpsVerifyNamesTheProviderThenTheFieldsThenIsPossible() {
    let m = wizard()
    #expect(reasonKey(m.verifyVpsPossibility()) == "onboarding.vps_config.status_pick_provider")
    m.selectVpsProvider(id: "hetzner")
    #expect(reasonKey(m.verifyVpsPossibility()) == "provisioning.fill_required_fields")
    m.setVpsCred(fieldId: "api-token", value: "filled")
    #expect(m.verifyVpsPossibility() == .possible)
}

// MARK: - provider-connect-button and the dialog's hosted-auth sign-in

@Test func providerConnectIsBlockedUntilFilledAndAbsentForAnUnknownProvider() {
    let m = wizard(handle: "alice@fresh.test")
    #expect(reasonKey(m.connectProviderPossibility(providerId: "cloudflare")) == "provisioning.fill_required_fields")
    m.setProviderCred(providerId: "cloudflare", fieldId: "api-token", value: "t")
    #expect(m.connectProviderPossibility(providerId: "cloudflare") == .possible)
    #expect(m.connectProviderPossibility(providerId: "no-such-provider") == .absent)
}

@Test func providerHostedAuthSignInWaitsForTheProviderAddress() {
    let m = wizard(handle: "alice@fresh.test")
    #expect(reasonKey(m.providerHostedAuthPossibility(providerId: "bundled", fieldId: "api-token"))
        == "provisioning.hosted_auth.base_url_first")
    m.setProviderCred(providerId: "bundled", fieldId: "base-url", value: "https://provider.example")
    #expect(m.providerHostedAuthPossibility(providerId: "bundled", fieldId: "api-token") == .possible)
}

// MARK: - the DNS / VPS forms' hosted-auth sign-in (CredentialsForm)

@Test func formHostedAuthSignInWaitsForTheProviderAddress() {
    let m = wizard()
    m.selectVpsProvider(id: "bundled")
    #expect(reasonKey(m.hostedAuthPossibility(form: .vps, fieldId: "api-token"))
        == "provisioning.hosted_auth.base_url_first")
    m.setVpsCred(fieldId: "base-url", value: "https://provider.example")
    #expect(m.hostedAuthPossibility(form: .vps, fieldId: "api-token") == .possible)
}

#if !FAUNA_KIDS
// MARK: - nest_retire: vps-verify-button and its sign-in

private func retire() -> NestRetireMachine {
    NestRetireMachine(
        candidateDomains: [], currentIpv4: nil, heldDnsProviderIds: [],
        heldDnsCredsJson: [], providerBaseUrl: nil, dohBaseUrl: nil
    )
}

@Test func retireVerifyNamesWhatRevivesItThenIsPossible() {
    let m = retire()
    #expect(reasonKey(m.verifyPossibility()) == "onboarding.vps_config.status_pick_provider")
    m.selectProvider(providerId: "hetzner")
    #expect(reasonKey(m.verifyPossibility()) == "provisioning.fill_required_fields")
    m.setCredentialField(fieldId: "api-token", value: "tok")
    m.expectAppInputs()
    #expect(reasonKey(m.verifyPossibility()) == "common.still_loading")
    m.appInputsLanded()
    #expect(m.verifyPossibility() == .possible)
}

@Test func retireHostedAuthSignInWaitsForTheProviderAddress() {
    let m = retire()
    m.selectProvider(providerId: "bundled")
    #expect(reasonKey(m.hostedAuthPossibility(fieldId: "api-token"))
        == "provisioning.hosted_auth.base_url_first")
    m.setCredentialField(fieldId: "base-url", value: "https://provider.example")
    #expect(m.hostedAuthPossibility(fieldId: "api-token") == .possible)
}
#endif

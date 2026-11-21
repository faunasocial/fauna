import Foundation
import Testing
@testable import FaunaKit

// The web hosting answers apple paints and re-asks
// (`docs/goal/behavior/web-content-hosting.md` § Published-post management,
// the subdomain toggle ruling, and `docs/goal/behavior/admin.md` § 7 Web):
// these pin that each answer the views read is the shared one and that each
// press refuses out loud with its reason. The predicates' own grammar is
// pinned in `fauna_client_web`.

private func key(_ p: Possibility) -> String {
    switch p {
    case .possible: return "<possible>"
    case .absent: return "<absent>"
    case .blocked(let reason): return reason.key
    }
}

@MainActor
private func hydrated(enabled: Bool, handle: String?, domain: String) -> WebPublishStore {
    let store = WebPublishStore()
    store.landHydrate(
        generation: store.currentGeneration, client: nil, servingDomain: domain,
        view: webSubdomainView(enabled: enabled, handle: handle, domain: domain),
        domains: [], posts: [])
    return store
}

@Suite @MainActor struct WebOfferOnlyPossibleTests {
    @Test func theToggleWaitsForItsRead() {
        let store = WebPublishStore()
        #expect(store.subdomainToggleAnswer == readPossibility(read: .pending))
        #expect(!store.subdomainToggleAnswer.isPossible)

        struct Boom: Error {}
        store.landHydrateFailure(generation: store.currentGeneration, error: Boom())
        #expect(store.subdomainToggleAnswer == readPossibility(read: .failed))
        #expect(store.errorMessage?.isEmpty == false, "a failed read reaches error-message")
    }

    @Test func switchingOnIsAbsentWithNoServingDomain() {
        let store = hydrated(enabled: false, handle: "alex", domain: "")
        #expect(key(store.subdomainToggleAnswer) == "<absent>")
    }

    @Test func switchingOnWithNoHandleIsBlockedWithTheStepAndThePressRefuses() async {
        let store = hydrated(enabled: false, handle: nil, domain: "web.test")
        #expect(key(store.subdomainToggleAnswer) == "web_settings.subdomain_no_handle")
        // No client is held, so a press that reached the nest could not land;
        // the refusal is the press asking the paint's answer again.
        await store.setSubdomainEnabled(true)
        #expect(store.errorMessage == store.subdomainToggleAnswer.refusal)
        #expect(store.errorMessage?.isEmpty == false)
        #expect(store.view?.enabled == false)
    }

    @Test func switchingOffIsAlwaysPossible() {
        let store = hydrated(enabled: true, handle: nil, domain: "")
        #expect(store.subdomainToggleAnswer.isPossible)
    }

    @Test func theCopyButtonsAnswerFromTheResolvedOrigin() {
        let off = hydrated(enabled: false, handle: "alex", domain: "web.test")
        #expect(key(off.copyLinkAnswer) == "web_settings.link_disabled_subdomain_off")
        #expect(off.refuseCopy())
        #expect(off.errorMessage == off.copyLinkAnswer.refusal)

        // An active custom domain resolves the origin (precedence: custom
        // domain > subdomain), with no handle needed.
        let on = WebPublishStore()
        on.landHydrate(
            generation: on.currentGeneration, client: nil, servingDomain: "web.test",
            view: webSubdomainView(enabled: false, handle: nil, domain: "web.test"),
            domains: [WebDomainRow(domain: "me.example", status: "active")], posts: [])
        #expect(on.copyLinkAnswer.isPossible)
        #expect(!on.refuseCopy())
    }

    @Test func theApexPickerWaitsForItsReadAndThePickRefuses() async {
        let vm = AdminWebVM()
        #expect(vm.apexPickerAnswer == readPossibility(read: .pending))
        await vm.select(index: 0)
        #expect(vm.errorMessage == vm.apexPickerAnswer.refusal)
        #expect(vm.errorMessage?.isEmpty == false)
    }

    @Test func theApexInfoNeverComposesAnEmptyAddress() {
        let vm = AdminWebVM()
        vm.apply(current: nil, users: [])
        #expect(vm.apexInfo == renderLocalizedText(webApexInfoText(domain: "")))
        #expect(!vm.apexInfo.contains("https:///"))
        #expect(!vm.apexInfo.isEmpty)
    }
}

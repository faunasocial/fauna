#if !FAUNA_KIDS
import Testing
@testable import FaunaKit

// A modal feed form's refusal or failed save shows on that form's own
// `error-message`, and the page's stands down while it is open (`feed.md`
// § Errors & edge cases; `ui/README.md` § Offer only what is possible, rule 7).
// On apple the modal feed forms are the reply and compose sheets; each reports
// its appearance to `FeedVM` (`modalFormOpened`/`modalFormClosed`). Nest-free:
// the refusals are answered by the shared draft predicates before any manager
// is consulted.

@MainActor
@Suite struct FeedModalFormErrorTests {
    @Test func replySheetRefusalShowsOnTheSheetAndThePageStandsDown() async {
        let vm = FeedVM()
        vm.setClientErrorMessage("page reason")
        vm.modalFormOpened(.reply)
        #expect(vm.errorMessage == nil)  // stood down while the sheet is open

        let saved = await vm.replyToPost(postId: "p1", body: "   ")
        #expect(saved == false)  // the sheet stays open
        let refusal = vm.replySubmitPossibility("   ").refusal
        #expect(refusal != nil)
        #expect(vm.modalFormError(.reply) == refusal)
        #expect(vm.modalFormError(.compose) == nil)
        #expect(vm.errorMessage == nil)
        #expect(vm.clientErrorMessage == "page reason")  // not overwritten

        vm.modalFormClosed(.reply)
        #expect(vm.modalFormError(.reply) == nil)
        #expect(vm.errorMessage == "page reason")  // the page's shows again
    }

    @Test func composeSheetRefusalShowsOnTheSheetAndThePageStandsDown() {
        let vm = FeedVM()
        vm.modalFormOpened(.compose)
        #expect(vm.refuseForm(vm.postSubmitPossibility) == true)  // empty draft
        let refusal = vm.postSubmitPossibility.refusal
        #expect(refusal != nil)
        #expect(vm.modalFormError(.compose) == refusal)
        #expect(vm.modalFormError(.reply) == nil)
        #expect(vm.errorMessage == nil)
        #expect(vm.clientErrorMessage == nil)

        // Reopening starts the sheet's line empty.
        vm.modalFormClosed(.compose)
        vm.modalFormOpened(.compose)
        #expect(vm.modalFormError(.compose) == nil)
    }

    // The compose sheet's Post: a save that fails (here no manager yet, the
    // nest-free failure) shows on the sheet's own `error-message` as well as
    // `compose-error`, the page's standing down — as linux's compose dialog.
    @Test func composeSheetFailedSaveShowsOnTheSheetAndComposeError() async {
        let vm = FeedVM()
        vm.setClientErrorMessage("page reason")
        vm.modalFormOpened(.compose)
        let sent = await vm.submitComposeForm()
        #expect(sent == false)  // the sheet stays open
        #expect(vm.composeError != nil)
        #expect(vm.modalFormError(.compose) == vm.composeError)
        #expect(vm.errorMessage == nil)
        #expect(vm.clientErrorMessage == "page reason")

        vm.modalFormClosed(.compose)
        #expect(vm.errorMessage == "page reason")
    }

    @Test func withNoSheetOpenARefusalIsThePages() {
        let vm = FeedVM()
        #expect(vm.refuseForm(vm.postSubmitPossibility) == true)  // the inline composer
        #expect(vm.errorMessage == vm.postSubmitPossibility.refusal)
        #expect(vm.modalFormError(.compose) == nil)
    }

    // Create-feed and bridge-subscribe are inline on apple: a save that fails
    // (here no manager yet, the nest-free failure) says so on the page's line —
    // never a silent `false` — and on an open sheet's line instead while one is
    // up, the page's standing down.
    @Test func inlineFeedFormsFailedSaveIsThePagesUnlessASheetIsOpen() async {
        let vm = FeedVM()
        let created = await vm.createFeed(name: "news", combination: "all", rules: [])
        #expect(created == false)
        #expect(vm.errorMessage == L.errors.feedNotReady)

        let subscribed = await vm.subscribeBridge(
            bridge: "bluesky", feedUri: "at://feed", name: "")
        #expect(subscribed == false)
        #expect(vm.errorMessage == L.errors.feedNotReady)

        let sheet = FeedVM()
        sheet.modalFormOpened(.compose)
        _ = await sheet.createFeed(name: "news", combination: "all", rules: [])
        #expect(sheet.errorMessage == nil)
        #expect(sheet.modalFormError(.compose) == L.errors.feedNotReady)
    }

    @Test func closingAFormThatIsNotOpenChangesNothing() {
        let vm = FeedVM()
        vm.modalFormOpened(.reply)
        _ = vm.refuseForm(vm.replySubmitPossibility(""))
        vm.modalFormClosed(.compose)
        #expect(vm.openModalForm == .reply)
        #expect(vm.modalFormError(.reply) != nil)
    }
}
#endif // !FAUNA_KIDS

import Foundation
import Testing
@testable import FaunaKit

// The macOS+iOS leg of the device-cap refusal notice (`docs/goal/ui/devices.md`
// § Errors & edge cases) — `DevicesMachineVM.enrollmentNotice`
// / `loadEnrollmentNotice()` over a recording `APIClient` double, mirroring
// `BackupDestinationsVMTests`'/`PushManagerDropActorRowTests`' `RecordingAPI`
// pattern. Pins exactly the do/catch-vs-plain-assignment distinction a
// review corrected: `accountEnrollmentNotice()` is non-throwing, so a
// plain assignment already gives "a successful `nil` clears the field" — there
// is no thrown-error arm to distinguish from a successful empty read here
// (unlike `loadCustodyFacet`'s `try?` fold over a throwing call), so that case
// is not representable and is not tested.

private struct StubDevicesMachineError: Error {}

private final class RecordingEnrollmentAPI: APIClient {
    var notice: String?

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    // Fails fast and offline — this test only exercises `loadEnrollmentNotice`,
    // called directly, never through a built `machine`.
    override func devicesMachine(observer: DevicesObserver) async throws -> DevicesMachine {
        throw StubDevicesMachineError()
    }

    override func accountEnrollmentNotice() async -> String? {
        notice
    }
}

@MainActor private func wired() -> (DevicesMachineVM, RecordingEnrollmentAPI) {
    let api = RecordingEnrollmentAPI()
    let vm = DevicesMachineVM()
    return (vm, api)
}

@Test @MainActor func loadEnrollmentNoticeSetsTheNoticeWhenSomeIsReturned() async {
    let (vm, api) = wired()
    await vm.configure(api: api)
    api.notice = "S.error.sync.device_limit_exceeded"

    await vm.loadEnrollmentNotice()

    #expect(vm.enrollmentNotice == "S.error.sync.device_limit_exceeded")
}

@Test @MainActor func loadEnrollmentNoticeClearsOnASuccessfulNilRead() async {
    let (vm, api) = wired()
    await vm.configure(api: api)
    api.notice = "S.error.sync.device_limit_exceeded"
    await vm.loadEnrollmentNotice()
    #expect(vm.enrollmentNotice != nil)

    // The remedy (a slot freeing + a register) clears the standing notice —
    // a later load actually returning `nil` must clear the field, not leave
    // the stale notice painted forever (the `try?`-flattening trap this row's
    // pitfall analysis warns against would fail exactly this assertion).
    api.notice = nil
    await vm.loadEnrollmentNotice()

    #expect(vm.enrollmentNotice == nil)
}

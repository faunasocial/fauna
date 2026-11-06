import Foundation
import Testing
@testable import FaunaKit

// The tier_1 pin for the Devices-page rename affordance on macOS and iOS
// (`ui/devices.md` § Device names): `DeviceCard` paints exactly the controls
// `DeviceRenameAffordance.controlIds` names, so this is the card's element
// sequence with the editor closed and open. tui is the reference
// (`devices.rs`, `the_rename_editor_opens_over_its_own_card_and_the_others_keep_the_button`):
// the open editor replaces the card's rename button. The journey itself —
// prefill, confirm, the name read on both seats — is the e2e
// `test_device_names.py::test_a_device_renamed_on_one_seat_reads_its_new_name_on_both`.
@Suite("Device rename affordance (the card's element sequence)")
struct DeviceRenameAffordanceTests {
    @Test("closed: the card carries the rename button and no editor")
    func closedPaintsTheButtonOnly() {
        #expect(DeviceRenameAffordance.controlIds(renaming: false) == ["device-rename-button"])
    }

    @Test("open: input, confirm, cancel in that order, and the button gives way")
    func openPaintsTheEditorInOrder() {
        #expect(
            DeviceRenameAffordance.controlIds(renaming: true) == [
                "device-rename-input",
                "device-rename-confirm-button",
                "device-rename-cancel-button",
            ]
        )
    }
}

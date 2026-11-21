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
        #expect(DeviceRenameAffordance.controlIds(renaming: false, offered: true) == ["device-rename-button"])
    }

    @Test("open: input, confirm, cancel in that order, and the button gives way")
    func openPaintsTheEditorInOrder() {
        #expect(
            DeviceRenameAffordance.controlIds(renaming: true, offered: true) == [
                "device-rename-input",
                "device-rename-confirm-button",
                "device-rename-cancel-button",
            ]
        )
    }

    // The nest's WebDAV pseudo-device row: the shared answer is Absent, so the
    // closed card carries no rename control at all (tui's
    // `the_webdav_pseudo_device_row_offers_no_rename`; `ui/README.md` § Offer
    // only what is possible). The answer itself is shared Rust, pinned by
    // `devices_lifecycle.rs::the_webdav_pseudo_device_row_offers_no_rename`.
    @Test("not offered: the closed card carries no rename control")
    func notOfferedPaintsNothing() {
        #expect(DeviceRenameAffordance.controlIds(renaming: false, offered: false).isEmpty)
    }
}

import Testing
@testable import FaunaKit

@Test @MainActor func keyPackageLowWhenBelowThreshold() {
    let vm = EncryptionSettingsVM()
    vm.keyPackageCount = 3
    #expect(vm.isKeyPackageLow == true)
}

@Test @MainActor func keyPackageNotLowWhenAtThreshold() {
    let vm = EncryptionSettingsVM()
    vm.keyPackageCount = 5
    #expect(vm.isKeyPackageLow == false)
}

@Test @MainActor func keyPackageNotLowWhenAboveThreshold() {
    let vm = EncryptionSettingsVM()
    vm.keyPackageCount = 10
    #expect(vm.isKeyPackageLow == false)
}

@Test @MainActor func keyPackageNotLowWhenNil() {
    let vm = EncryptionSettingsVM()
    vm.keyPackageCount = nil
    #expect(vm.isKeyPackageLow == false)
}

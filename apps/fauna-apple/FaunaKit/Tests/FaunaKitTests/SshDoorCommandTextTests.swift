import Testing
@testable import FaunaKit

// `provisioning-ssh-door-command`'s text (`docs/goal/behavior/onboarding-provisioning.md`
// § 5): the command with the one admitted address — or the "address you set the
// server up from" wording when the claim reply echoed none — the key path on a
// desktop app, and the fingerprint. The e2e witness is
// `test_ssh_door_wizard.py` (macos and ios).

private func readout(keyPath: String?, admittedFrom: String?) -> SshDoorReadout {
    SshDoorReadout(
        domain: "box.example",
        command: keyPath == nil ? "ssh -i fauna-box.example root@box.example" : "ssh box.example",
        keyPath: keyPath,
        fingerprint: "SHA256:abc",
        admittedFrom: admittedFrom)
}

@Test func desktopCommandNamesTheKeyPathAndTheAdmittedAddress() {
    let text = sshDoorCommandText(readout(keyPath: "/Users/u/.ssh/fauna-box.example", admittedFrom: "203.0.113.7"))
    #expect(text.contains("ssh box.example"))
    #expect(text.contains("/Users/u/.ssh/fauna-box.example"))
    #expect(text.contains("203.0.113.7"))
    #expect(text.contains("SHA256:abc"))
}

@Test func handOverCommandHasNoPathAndFallsBackWhenNoAddressWasEchoed() {
    let text = sshDoorCommandText(readout(keyPath: nil, admittedFrom: nil))
    #expect(text.contains("ssh -i fauna-box.example root@box.example"))
    #expect(text.contains(L.onboarding.nestProvisioning.sshDoorAddressUnknown))
    #expect(text.contains("SHA256:abc"))
    #expect(!text.contains("/.ssh/"))
}

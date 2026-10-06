import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `enable_caldav_mailbox` — mint the currently logged-in actor's shared MSEK
/// + `default` mail credential via `MailSettingsMachine.enableCaldavMailbox{With
/// Password,WithGeneratedPassword}` (the same read-only-mailbox recipe linux's
/// `enable_caldav_mailbox_for_test` drives), so a stock CalDAV client (or the
/// admin-CalDAV-port-rebind test's own raw PUT) can AUTH as this actor without
/// going through the mail-settings UI. Writes the outcome into
/// `TestAgentReplies.caldavMailboxReply` (`tests/e2e-unified/helpers/
/// mail_dedicated_nest.py::mint_caldav_mailbox` polls it — the same
/// `{"ok": true}` / `{"ok": false, "error": ...}` shape linux serializes).
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2) —
/// was a byte-identical per-target twin until this harvest pass found it
/// . Takes `client` directly rather
/// than `AppState`/`MacAppState` (no common protocol between them).
public enum CaldavMailboxTestCommand {
    @MainActor
    public static func apply(_ command: [String: Any], client: FaunaClient?) async {
        TestAgentReplies.caldavMailboxReply = nil
        guard let api = client?.api else {
            TestAgentReplies.caldavMailboxReply = ["ok": false, "error": "fauna client not initialized"]
            return
        }
        let password = command["password"] as? String
        do {
            let machine = try await api.mailSettingsMachine()
            if let password {
                try await machine.enableCaldavMailboxWithPassword(displayName: "Default", password: password)
            } else {
                _ = try await machine.enableCaldavMailboxWithGeneratedPassword(displayName: "Default")
            }
            TestAgentReplies.caldavMailboxReply = ["ok": true]
        } catch {
            TestAgentReplies.caldavMailboxReply = ["ok": false, "error": "\(error)"]
        }
    }
}

#endif

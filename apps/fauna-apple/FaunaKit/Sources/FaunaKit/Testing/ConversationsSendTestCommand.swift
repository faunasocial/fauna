import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// Shared handler for the conversations TestAgent commands
/// `ConversationsRealMembershipTestCommand` deliberately doesn't cover: minting
/// an MLS group, injecting a synthetic send failure, selecting a message, and
/// the two *send* arms (`conversations_real_resolve_send_new`,
/// `conversations_real_send`).
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2) —
/// these five were BYTE-IDENTICAL per-target twins until this harvest pass
/// found them, the same shape
/// `ConversationsRealMembershipTestCommand` unified for the membership arms on
/// 2026-08-28. The two *send* arms return a refusal reason rather than calling
/// `testAgentFailure` themselves, mirroring that file's convention — the
/// caller surfaces it loudly (convention 11's declining-arm clause).
public enum ConversationsSendTestCommand {
    @MainActor
    public static func createMlsGroup(_ command: [String: Any], vm: ConversationsVM) {
        let participants: [TypedAddress] = (command["participants"] as? [String] ?? []).map {
            .fauna(handle: $0, actorId: Data(repeating: 0, count: 32))
        }
        let threadId = vm.manager.createMlsGroup(participants: participants)
        vm.manager.selectThread(id: threadId)
    }

    @MainActor
    public static func injectSendFailure(_ command: [String: Any], vm: ConversationsVM) {
        let threadId = (command["thread_id"] as? String) ?? ""
        if threadId.isEmpty {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] conversations_inject_send_failure: missing thread_id")
            return
        }
        let rawReason = (command["reason"] as? String) ?? ""
        let reason = rawReason.isEmpty ? "nest rejected fauna.email.send" : rawReason
        vm.manager.injectSendFailureForTest(id: threadId, reason: reason)
    }

    /// `conversations_select_message` — drive `ConversationsVM.selectThreadAndMessage`
    /// directly, the same call `SearchResultsView`'s `Mail` row tap makes. iOS's
    /// local content index is query-only (`CLIENT_BUILDS_INDEX` is false there,
    /// `libs/fauna-ffi/src/index_launch.rs`), so a single-seat test can never
    /// build a segment to search and reach this call through a real Search
    /// result — this seam isolates the downstream paint/scroll half of the
    /// contract so it is provable on a real simulator without a two-device
    /// sync run (`docs/goal/ui/conversations.md` § The selected message).
    @MainActor
    public static func selectMessage(_ command: [String: Any], vm: ConversationsVM) {
        let threadId = (command["thread_id"] as? String) ?? ""
        let messageId = (command["message_id"] as? String) ?? ""
        if threadId.isEmpty || messageId.isEmpty {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] conversations_select_message: missing thread_id/message_id")
            return
        }
        vm.selectThreadAndMessage(threadId, messageId)
    }

    /// `conversations_real_resolve_send_new` — drive a **real** FaunaMls send the way
    /// the GUI does: type the recipient, resolve it through the real backend probe
    /// (`fauna.conversations.keypackage.count` promotes a 64-hex actor id to a Fauna
    /// address), commit the chip, then `sendNewThread` bootstraps the group (fetch key
    /// package → create MLS group → deliver Welcome → post Application envelope). The
    /// one gesture that WRITES the MLS state store (the account-scoped
    /// `Fauna/<actor-id-hex>/mls.db`, `AccountStateDir`).
    @MainActor
    public static func resolveSendNew(_ command: [String: Any], vm: ConversationsVM) async -> String? {
        let recipient = command["recipient"] as? String ?? ""
        let body = command["body"] as? String ?? ""
        let m = vm.manager
        m.startNewConversation()
        m.setNewThreadRecipientInput(text: recipient)
        await m.resolveRecipient()
        guard m.acceptCurrentRecipientChip() else {
            // Which of the probe's outcomes left no chip (convention 6): `error`
            // is a failed FaunaMls probe, `notFound` a probe that never
            // confirmed the id (including no FaunaMls backend registered on the
            // manager yet), `resolving` a probe still pending.
            // `resolving` after the await means the probe's result did not
            // survive to the accept: an add-participant overlay was the active
            // picker (it takes priority), or the input was re-set under it (the
            // apple picker once echoed every model write back as a user edit,
            // which cleared `resolved` — `RecipientPicker.swift`).
            let snap = m.snapshot()
            let picker = snap.newThreadCompose?.recipientPicker
            let state = picker.map { "\($0.resolveState)" } ?? "no new-thread picker open"
            let input = picker.map { "'\($0.rawInput)'" } ?? "-"
            return "conversations_real_resolve_send_new: recipient '\(recipient)' did not resolve "
                + "to a chip (not a reachable Fauna actor?) [resolve_state=\(state) "
                + "raw_input=\(input) add_participant_open=\(snap.addParticipant != nil)]"
        }
        m.setNewThreadBody(body: body)
        do {
            _ = try await m.sendNewThread()
        } catch {
            return "conversations_real_resolve_send_new: send failed: \(error)"
        }
        return nil
    }

    /// `conversations_real_send` — send `body` on an **existing** thread over the
    /// real rail (`ConversationsManager.send`; a forked-but-unbound FaunaMls group
    /// bootstraps its MLS group lazily here).
    @MainActor
    public static func realSend(_ command: [String: Any], vm: ConversationsVM) async -> String? {
        guard let threadId = command["thread_id"] as? String, !threadId.isEmpty else {
            return "conversations_real_send: missing thread_id"
        }
        let body = command["body"] as? String ?? ""
        let m = vm.manager
        m.setComposeBody(id: threadId, body: body)
        do {
            try await m.send(id: threadId)
        } catch {
            return "conversations_real_send: send failed: \(error)"
        }
        return nil
    }
}

#endif

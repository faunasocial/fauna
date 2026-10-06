import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// Shared handler for the three **real-wire** conversations membership/rename
/// TestAgent commands — `conversations_real_add`, `conversations_real_remove`,
/// `conversations_real_rename` (`e2e-conventions.md` § convention 11: an
/// agent's command table is a cross-app contract).
///
/// Until 2026-08-28 both apple shells fell to their `default:` catch-all on all
/// three while every other shell carried them (android
/// `TestAgent.kt::conversations_real_*`, windows, linux, tui), so the two
/// `real_conversations` drivers — `test_fauna_mls_real_roundtrip` and
/// `test_thread_membership_real.py::test_in_place_mls_add_through_the_ui` —
/// could not reach apple at all. Apple already had the two *send* arms
/// (`conversations_real_resolve_send_new`, `conversations_real_send`); these are
/// the membership half.
///
/// **No `libs/fauna-ffi` work was owed**: all six manager methods these arms
/// drive were already inside `ConversationsManager`'s `uniffi::export` blocks
/// (`libs/fauna-conversations/src/manager.rs` — `open_add_participant`,
/// `set_add_participant_recipient_input`, `accept_add_participant_chip`,
/// `confirm_add_participant`, `rename_thread`, `remove_participant`), which is
/// what let android land all eight arms in one commit. The apple arms are glue
/// over the same shared Rust, not a second implementation of anything.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2),
/// mirroring `BackupAuditTestCommand` / `DelegationClockTestCommand`. It takes
/// the shell's `ConversationsVM` as a parameter rather than reading a
/// `liveInstanceForTest` static, because the conversations VM is a shell-owned
/// `@StateObject` with no such static and inventing one would add a second way
/// to reach the same object.
///
/// Every refusal is returned as a reason string for the caller to surface
/// **loudly** through `AppMessages.reportRefusedAgentCommand` — never a silent
/// no-op (convention 11's declining-arm clause).
public enum ConversationsRealMembershipTestCommand {
    /// Apply `action`. Returns `nil` on success, or a human-readable reason the
    /// caller must surface as a loud TestAgent failure.
    ///
    /// Mirrors android's payload contract exactly (`TestAgent.kt`), because the
    /// cross-app action layer sends one payload to every app:
    /// `thread_id` plus, for add/remove, `peer_handle` + `peer_actor_id_hex`;
    /// `label` for rename.
    @MainActor
    public static func apply(
        _ action: String,
        _ command: [String: Any],
        vm: ConversationsVM
    ) async -> String? {
        guard let threadId = command["thread_id"] as? String, !threadId.isEmpty else {
            return "\(action) carried no `thread_id`"
        }

        switch action {
        case "conversations_real_add":
            // On a bound FaunaMls group this posts the MLS Commit + Welcome; on
            // a 1:1 it forks a fresh group (snapshot-only until its first
            // real_send). Same four-step picker drive android performs, because
            // the add goes through the real add-participant overlay rather than
            // a back door: `confirm_add_participant` is what posts the wire op.
            guard let addr = peerAddress(action, command) else {
                return hexRefusal(action, command)
            }
            vm.openAddParticipant(threadId)
            vm.setAddParticipantRecipientInput(addr.handleForInput)
            vm.acceptAddParticipantChip(addr.typed)
            _ = await vm.confirmAddParticipant()
            return wireRefusal(action, vm)

        case "conversations_real_remove":
            // `peer_handle` must match the one used at real_add: the snapshot
            // removal keys on `TypedAddress::display()` (the handle), while the
            // wire op finds the MLS leaf by actor_id. Android's arm carries the
            // same warning for the same reason.
            guard let addr = peerAddress(action, command) else {
                return hexRefusal(action, command)
            }
            await vm.removeParticipant(threadId, addr.typed)
            return wireRefusal(action, vm)

        case "conversations_real_rename":
            // Posts the encrypted `GroupMeta::NameChanged` Application envelope.
            await vm.renameThread(threadId, command["label"] as? String ?? "")
            return wireRefusal(action, vm)

        default:
            // Unreachable via `actions`, but a wrong-command refusal must still
            // name itself rather than succeed vacuously.
            return "\(action) is not a conversations real-membership command"
        }
    }

    /// The manager's page error after a membership/rename op, as a named
    /// refusal — or `nil` when the op left the page clean.
    ///
    /// Convention 11's declining-arm clause, and the half a straight port of
    /// android's arm would have missed: all three manager methods swallow their
    /// wire failure into `snapshot.error` and return normally, so an arm that
    /// only awaits them acks GREEN on a refused add. linux's `e2e_add` has
    /// checked this since it landed (`conv_backend.rs::page_error`), and it is
    /// what turns "the count never reached 3" — a symptom read ~20 s later by
    /// the action layer's own poll — into the nest's actual sentence, attributed
    /// to the command that caused it.
    ///
    /// ⚠ **Unproven on apple as of 2026-08-28: it did NOT fire on the one run
    /// that should have fired it.** `test_in_place_mls_add_through_the_ui`
    /// `--app macos` failed with the manager having logged
    /// `conversations page error: conversations.unified.error_add_participant`
    /// during the awaited confirm, yet `vm.pageError` read empty here
    /// immediately afterwards and no `[TestAgent]` line reached the log. Two
    /// candidates, neither checked: the snapshot's `error` is published a tick
    /// later than the `await` returns, or `renderLocalizedText` yields "" for
    /// that key on apple (`ConversationsVM.pageError` returns the RENDERED
    /// string, so a missing catalog entry is indistinguishable from no error).
    /// Diagnose before relying on this arm's refusal; it is kept because it is
    /// linux's shape and costs nothing when silent, not because it is proven.
    @MainActor
    private static func wireRefusal(_ action: String, _ vm: ConversationsVM) -> String? {
        let error = vm.pageError
        return error.isEmpty ? nil : "\(action): \(error)"
    }

    // ── payload parsing ─────────────────────────────────────────────────────

    private struct PeerAddress {
        let handleForInput: String
        let typed: TypedAddress
    }

    /// `peer_handle` + a 64-char hex `peer_actor_id_hex` → the `TypedAddress`
    /// the manager's picker and removal both key on. `nil` when the hex is not
    /// a 32-byte actor id, which the caller turns into a named refusal.
    private static func peerAddress(_ action: String, _ command: [String: Any]) -> PeerAddress? {
        let handle = command["peer_handle"] as? String ?? ""
        guard let actorId = Data(hexString: command["peer_actor_id_hex"] as? String ?? ""),
              actorId.count == 32
        else { return nil }
        return PeerAddress(
            handleForInput: handle,
            typed: .fauna(handle: handle, actorId: actorId)
        )
    }

    private static func hexRefusal(_ action: String, _ command: [String: Any]) -> String {
        let raw = command["peer_actor_id_hex"] as? String ?? ""
        return "\(action)'s `peer_actor_id_hex` is not a 64-char hex actor id: '\(raw)'"
    }
}

#endif

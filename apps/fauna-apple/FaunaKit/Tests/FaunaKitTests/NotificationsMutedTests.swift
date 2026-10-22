import Testing
@testable import FaunaKit

// The apple leg of `docs/goal/behavior/notifications.md` § Muted words: the
// page's collapse is the shared `notification_muted` decision over the row's
// relayed `body.args` and the process-held list (`MutedWordsHolder`), and the
// reveal is per-row and session-local. FFI-pure — no nest.

private func row(id: String, type: String, key: String, args: [String: String]) -> NotificationItem {
    NotificationItem(id: id, body: "painted sentence", summary: "\(type) row",
                     createdAt: 1_700_000_000_000, read: false, notifType: type,
                     source: "fauna", localizedBody: LocalizedText(key: key, args: args))
}

private let spoiler = [MutedKeyword(keyword: "spoiler", weight: -1000)]

@MainActor
@Test func aRowWhoseRelayedTextCarriesAMutedWordCollapsesUntilRevealed() {
    MutedWordsHolder.shared.publish(spoiler)
    NotificationsVM.revealedMutedIds = []
    defer { MutedWordsHolder.shared.clear(); NotificationsVM.revealedMutedIds = [] }
    let vm = NotificationsVM()
    let knock = row(id: "1", type: "knock", key: "notifications.row_knock",
                    args: ["sender": "k1", "message": "a SPOILER ahead"])
    let like = row(id: "2", type: "like", key: "notifications.row_like", args: ["sender": "clean"])

    #expect(vm.isCollapsed(knock))
    #expect(!vm.isCollapsed(like), "a row whose args carry no muted word paints in full")

    vm.revealMuted(knock)
    #expect(!vm.isCollapsed(knock), "the reveal paints the row in full for the session")
    #expect(NotificationsVM().isCollapsed(knock) == false,
            "the reveal outlives the page's own view model")
}

@MainActor
@Test func aSecurityNoticeAndAnEmptyListNeverCollapse() {
    MutedWordsHolder.shared.publish(spoiler)
    defer { MutedWordsHolder.shared.clear() }
    let vm = NotificationsVM()
    let notice = row(id: "3", type: "security.notice",
                     key: "notifications.row_security_archive_exported",
                     args: ["ip": "203.0.113.7 spoiler"])
    #expect(!vm.isCollapsed(notice), "rule 5: a security notice renders in full")

    MutedWordsHolder.shared.clear()
    let knock = row(id: "4", type: "knock", key: "notifications.row_knock",
                    args: ["sender": "k1", "message": "spoiler"])
    #expect(!vm.isCollapsed(knock), "an empty list collapses nothing")
}

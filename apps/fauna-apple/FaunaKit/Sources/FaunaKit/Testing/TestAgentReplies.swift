import Foundation

/// Reply slots for test-agent commands that mint state off-screen and report an
/// outcome back to the driver via `/app/state` — mirrors `AppMessages`' shape
/// (a shared holder `serializeState()` reads) but for one-shot command replies
/// rather than persistent UI messages. `caldavMailboxReply` is read by the
/// `enable_caldav_mailbox` test-agent command handler's caller
/// (`tests/e2e-unified/helpers/mail_dedicated_nest.py::mint_caldav_mailbox`,
/// the exact linux `caldav_mailbox_reply` shape: `{"ok": true}` or
/// `{"ok": false, "error": "..."}`). `webdavServeReply` mirrors linux's
/// `webdav_serve_reply` for `tests/e2e-unified/helpers/webdav_roundtrip.py
/// ::serve_enable_folder` (`{"ok": true, "served_sets": N}` or
/// `{"ok": false, "error": "..."}`).
@MainActor
public final class TestAgentReplies {
    public static var caldavMailboxReply: [String: Any]?
    public static var webdavServeReply: [String: Any]?
}

//! Advisory: project-shipped kinds match `fauna.<area>.<verb>`.
//! Forks pick their own prefix; this test exempts known fork prefixes
//! (none yet) and only flags upstream-shipped kinds.

use regex::Regex;

const UPSTREAM_KINDS: &[&str] = &[
    "fauna.knock",
    "fauna.account.update",
    "fauna.notification",
    "fauna.peer.wake",
    "fauna.peer.signal",
    "fauna.peer.node_info",
    "fauna.peer.exchange",
    "fauna.calendar.changed",
    "fauna.addressbook.changed",
    "fauna.conversations.channel.message",
    "fauna.conversations.welcome.received",
    "fauna.mail.received",
    "fauna.mail.flags_changed",
    "fauna.push.notification",
    "fauna.push.presence",
    "fauna.sync.serve.announce",
    "fauna.sync.chunk.wanted",
    "fauna.inbox.item",
    "fauna.protocol.resync_required",
    "fauna.protocol.echo",
];

#[test]
fn upstream_kinds_match_namespace_policy() {
    // Pattern: fauna.<segment>(.<segment>)+ — at least 2 dotted segments.
    // Each segment lowercase, alphanumeric + underscore, starts with letter.
    // The spec lists 2-segment kinds (fauna.knock, fauna.notification) alongside
    // 3-segment ones (fauna.account.update, fauna.protocol.echo).
    let re = Regex::new(r"^fauna(\.[a-z][a-z0-9_]*)+$").unwrap();
    for kind in UPSTREAM_KINDS {
        assert!(
            re.is_match(kind),
            "upstream kind '{}' violates namespace policy (expected fauna.<area>.<verb>+)",
            kind
        );
    }
}

#[test]
fn upstream_kinds_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for kind in UPSTREAM_KINDS {
        assert!(seen.insert(*kind), "duplicate kind: {}", kind);
    }
}

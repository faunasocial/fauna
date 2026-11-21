//! What a shell lists: the tools its held connections' granted scopes reach
//! (`apps/cli.md` § One catalog, many front-ends → *A tool that cannot run*,
//! rule 2).
//!
//! Before any account is connected the shell lists the whole catalog — what
//! connecting opens, `connect` the one step that unblocks it. Once it holds
//! connections whose granted scope it knows (the token reply's `scope`, stored
//! with the connection) it lists only the tools whose arms at least one of
//! them reaches. Authority and preconditions are never a listing filter: they
//! change under a session and are answered at the call (rule 3).
//!
//! Wasm-safe and shell-agnostic: the device shell and the nest-hosted shell
//! ask the same functions; only where the reach comes from differs.

use crate::catalog::{ARM_CONVERSATION_READ, ARM_CONVERSATION_SEND, CATALOG, Tool, is_admin_arm};

/// What the shell's held connections reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// No held connection's granted scope is known — none is held, or one
    /// is whose scope the shell never read: the whole catalog is listed.
    Whole,
    /// The granted scope of each held connection, each one's strings.
    Granted(Vec<Vec<String>>),
}

impl Reach {
    /// The reach of a shell holding these connections, each its granted
    /// scope where known. No connection, or any one whose scope is unknown,
    /// is [`Reach::Whole`]: a tool a held grant may reach is never hidden.
    pub fn of(scopes: impl IntoIterator<Item = Option<Vec<String>>>) -> Self {
        let mut granted = Vec::new();
        for scope in scopes {
            match scope {
                Some(scope) => granted.push(scope),
                None => return Reach::Whole,
            }
        }
        if granted.is_empty() {
            Reach::Whole
        } else {
            Reach::Granted(granted)
        }
    }

    /// Whether `tool` is reached: every one of its arms is covered by one
    /// held connection's scope. A tool the shell answers itself (no arm) is
    /// always reached.
    pub fn reaches(&self, tool: &Tool) -> bool {
        match self {
            Reach::Whole => true,
            Reach::Granted(scopes) => scopes
                .iter()
                .any(|scope| tool.arms.iter().all(|arm| scope_covers(scope, arm))),
        }
    }
}

/// The token reply's `scope` — space-separated strings (RFC 6749 §3.3).
pub fn parse_scope(scope: &str) -> Vec<String> {
    scope.split_whitespace().map(str::to_owned).collect()
}

/// Whether a granted `scope` covers `arm`: the string itself, or — for an
/// admin page's `read` — the page's `rw` string, a superset of its own
/// (`authorization-server.md` § Scope grammar → *The admin arms*).
pub fn scope_covers(scope: &[String], arm: &str) -> bool {
    if scope.iter().any(|s| s == arm) {
        return true;
    }
    match arm.strip_prefix("fauna:admin:read:") {
        Some(page) => {
            let rw = format!("fauna:admin:rw:{page}");
            scope.contains(&rw)
        }
        None => false,
    }
}

/// Whether `tool` needs the admin plane.
pub fn is_admin_tool(tool: &Tool) -> bool {
    tool.arms.iter().any(|arm| is_admin_arm(arm))
}

/// Whether `tool` is performed at the conversations oracle's door — never
/// possible on a shell that has none (rule 1), so such a shell never lists it.
pub fn needs_conversations_door(tool: &Tool) -> bool {
    tool.arms
        .iter()
        .any(|arm| *arm == ARM_CONVERSATION_READ || *arm == ARM_CONVERSATION_SEND)
}

/// The tools a shell lists, in catalog order: those `reach` reaches, minus
/// the conversations tools on a shell with no door.
pub fn listed(reach: &Reach, conversations_door: bool) -> Vec<&'static Tool> {
    CATALOG
        .iter()
        .filter(|t| conversations_door || !needs_conversations_door(t))
        .filter(|t| reach.reaches(t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::consent_scope;

    fn names(tools: &[&Tool]) -> Vec<&'static str> {
        tools.iter().map(|t| t.name).collect()
    }

    #[test]
    fn no_known_scope_is_the_whole_catalog() {
        assert_eq!(Reach::of([]), Reach::Whole);
        assert_eq!(
            Reach::of([Some(vec!["fauna:feed:read".into()]), None]),
            Reach::Whole
        );
        assert_eq!(listed(&Reach::Whole, true).len(), CATALOG.len());
    }

    #[test]
    fn a_member_grant_lists_no_admin_tool_and_an_admin_grant_lists_them() {
        let member = Reach::of([Some(parse_scope(&consent_scope(false)))]);
        let listed_member = listed(&member, true);
        assert!(listed_member.iter().all(|t| !is_admin_tool(t)));
        assert!(names(&listed_member).contains(&"connect"));
        assert!(names(&listed_member).contains(&"feed_list"));

        let admin = Reach::of([Some(parse_scope(&consent_scope(true)))]);
        assert_eq!(listed(&admin, true).len(), CATALOG.len());
    }

    #[test]
    fn the_union_of_held_grants_is_listed() {
        let feed_only = Some(vec!["fauna:feed:read".to_owned()]);
        let admin = Some(vec!["fauna:admin:read:dashboard".to_owned()]);
        let reach = Reach::of([feed_only, admin]);
        let n = names(&listed(&reach, true));
        assert!(n.contains(&"feed_local"));
        assert!(n.contains(&"admin_dashboard"), "{n:?}");
        // feed_list needs the account arm too, which no held grant carries.
        assert!(!n.contains(&"feed_list"));
    }

    #[test]
    fn an_admin_rw_string_covers_its_pages_read() {
        let scope = vec!["fauna:admin:rw:users".to_owned()];
        assert!(scope_covers(&scope, "fauna:admin:read:users"));
        assert!(scope_covers(&scope, "fauna:admin:rw:users"));
        assert!(!scope_covers(&scope, "fauna:admin:read:nest"));
        assert!(!scope_covers(
            &["fauna:admin:read:users".to_owned()],
            "fauna:admin:rw:users"
        ));
    }

    #[test]
    fn a_shell_without_a_door_lists_no_conversations_tool() {
        let n = names(&listed(&Reach::Whole, false));
        assert!(n.iter().all(|t| !t.starts_with("conversations_")), "{n:?}");
        assert!(names(&listed(&Reach::Whole, true)).contains(&"conversations_list"));
    }
}

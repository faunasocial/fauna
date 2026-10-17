//! The signed-in account's own `<handle>@<domain>` — the two resolutions every
//! app needs from one registry entry: the **send** address (the mail rail's
//! `From:`) and the **display** handle (the profile SELF header). Pure over the
//! session handle + the entry's cached handle/domain, so every app answers both
//! from the same table (priority #2) and the branch table is tested here.

/// The address this account may SEND as — `<handle>@<domain>` from the session
/// handle + the registry entry's cached handle/domain.
///
/// A handle that already carries an `@domain` **is** the address (the
/// onboarding-wizard path, where the entered handle is a full address).
/// Otherwise the address is the registry's cached handle at the registry's
/// cached domain — **both** must come from the nest.
///
/// The session handle is deliberately *not* a fallback for a missing registry
/// handle. The nest does sender-handle verification on every `fauna.email.send`
/// (`mail-app-surface.md` § First-party client send), so `<session-handle>@<domain>` for
/// an actor whose nest row has no handle is an address this account does not
/// own — the nest refuses it, and claiming it is what made every tui mail send
/// fail as `fauna.email.permission_denied` for an admin-admitted actor.
///
/// The "no handle yet" and "no handle at all" cases are distinguishable here
/// *because* the domain is required first: an entry's domain is written only
/// from a nest ceremony's reply, and that same reply carries the handle. So
/// reaching the handle lookup at all proves the nest has answered — an absent
/// handle there is the nest's answer, not a pending load.
pub fn self_address(
    session_handle: &str,
    entry_handle: Option<&str>,
    entry_domain: Option<&str>,
) -> Option<String> {
    if session_handle.contains('@') {
        return Some(session_handle.to_string());
    }
    registry_address(entry_handle, entry_domain)
}

/// The account's handle as the profile SELF header DISPLAYS it —
/// `<handle>@<current primary>` (`mail-primary-domain-rename.md` § Display-handle
/// interaction: the display follows the primary, read at render time, so a
/// rename moves it with no new alias).
///
/// The display twin of [`self_address`], differing only in order: the
/// registry's `<handle>@<domain>` comes FIRST, so a full-address session handle
/// (the onboarding path) is only the fallback until the nest has answered. The
/// mail rail keeps the entered address as its `From:`; the header shows the
/// handle on the nest's current primary, which a rename moves. Never the bare
/// handle: `None` while nothing resolves, and the caller falls back to the
/// actor id as it does today.
pub fn display_handle(
    session_handle: &str,
    entry_handle: Option<&str>,
    entry_domain: Option<&str>,
) -> Option<String> {
    registry_address(entry_handle, entry_domain).or_else(|| {
        session_handle
            .contains('@')
            .then(|| session_handle.to_string())
    })
}

/// `<handle>@<domain>` from the registry entry alone — both from the nest, or
/// nothing ([`self_address`] says why the session handle never stands in for a
/// missing one).
fn registry_address(entry_handle: Option<&str>, entry_domain: Option<&str>) -> Option<String> {
    let domain = entry_domain.map(str::trim).filter(|d| !d.is_empty())?;
    let handle = entry_handle.map(str::trim).filter(|h| !h.is_empty())?;
    Some(format!("{handle}@{domain}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── self_address (the mail rail's `From:` resolution) ───────────

    #[test]
    fn self_address_is_the_registry_handle_at_the_registry_domain() {
        // The returning-user shape: the silent challenge cached BOTH the nest's
        // handle and its domain on the registry entry → `<handle>@<domain>`.
        assert_eq!(
            self_address("alice", Some("alice"), Some("fauna.test")),
            Some("alice@fauna.test".to_string()),
        );
    }

    #[test]
    fn self_address_prefers_the_registry_handle_over_the_session_one() {
        // The nest's whoami handle wins when present (production); the session
        // handle is only the fallback.
        assert_eq!(
            self_address("e2e-user", Some("bob"), Some("fauna.test")),
            Some("bob@fauna.test".to_string()),
        );
    }

    #[test]
    fn self_address_is_none_without_a_domain() {
        // No cached domain yet (the gap this whole change closes): no address,
        // rather than a malformed `alice@` — the SMTP rail keeps its empty From.
        assert_eq!(self_address("alice", None, None), None);
        assert_eq!(self_address("alice", Some("bob"), Some("")), None);
    }

    #[test]
    fn self_address_passes_through_a_handle_that_already_carries_a_domain() {
        // The onboarding-wizard path: the entered handle is already a full
        // address, so it IS the From regardless of the registry entry.
        assert_eq!(
            self_address("alice@fauna.test", None, None),
            Some("alice@fauna.test".to_string()),
        );
        assert_eq!(
            self_address("alice@fauna.test", Some("bob"), Some("other.test")),
            Some("alice@fauna.test".to_string()),
        );
    }

    // ── display_handle (the profile SELF header's fallback) ─────────

    #[test]
    fn display_handle_follows_the_registry_domain_even_over_a_full_address() {
        // The onboarding path entered `alice@old.test`; after a primary rename
        // the nest's answer names the new primary, and the header follows it
        // (mail-primary-domain-rename.md § Display-handle interaction) while
        // the mail rail keeps the entered address.
        assert_eq!(
            display_handle("alice@old.test", Some("alice"), Some("new.test")),
            Some("alice@new.test".to_string()),
        );
        assert_eq!(
            self_address("alice@old.test", Some("alice"), Some("new.test")),
            Some("alice@old.test".to_string()),
        );
    }

    #[test]
    fn display_handle_on_the_relaunch_path_is_never_the_bare_handle() {
        // The relaunch path's session handle is the registry's bare `bob`.
        assert_eq!(
            display_handle("bob", Some("bob"), Some("fauna.test")),
            Some("bob@fauna.test".to_string()),
        );
        assert_eq!(display_handle("bob", Some("bob"), None), None);
        assert_eq!(
            display_handle("bob@fauna.test", None, None),
            Some("bob@fauna.test".to_string()),
        );
    }

    #[test]
    fn self_address_is_none_when_the_nest_reports_no_handle() {
        // A registered actor whose nest handle is empty has NO address it may
        // claim, and the session handle is not a substitute: the nest does
        // sender-handle verification on every `fauna.email.send`
        // (mail-app-surface.md § First-party client send), so `<session-handle>@<domain>`
        // is an address this actor does not own and the nest refuses it.
        //
        // The empty-vs-absent distinction is decidable here precisely because
        // the domain is present: `entry_domain` is written only from a nest
        // ceremony's reply, which carries the handle too, so reaching this
        // line proves the nest has answered and the nest genuinely reports no
        // handle — this is not a not-yet-loaded race.
        assert_eq!(self_address("e2e-user", Some(""), Some("fauna.test")), None);
        assert_eq!(self_address("e2e-user", None, Some("fauna.test")), None);
    }
}

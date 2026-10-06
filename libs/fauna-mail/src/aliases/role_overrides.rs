//! Per-domain role-address override map (`mail_domains.role_address_overrides`).
//!
//! The four RFC 2142 role local-parts whose per-domain delivery target the admin
//! may override — `postmaster` / `abuse` / `noc` / `security` — map to an actor
//! id here. An absent key falls back to the deployment admin
//! (`mail-multidomain.md` § Default routing). `tlsrpt` / `dmarc-report` are
//! deliberately **not** representable: they always route to the deployment-wide
//! report processor regardless of any override (`mail-multidomain.md`
//! § Per-domain override), so storing them would be a footgun.
//!
//! The typed shape is [`RoleAddressOverrides`], defined beside the wire row
//! that carries it (`fauna_protocol::bridge_routing`) and re-exported here. This
//! module owns the column's one **at-rest encoding** (ratified
//! `mail-multidomain.md` § Wire shape + storage): a JSON object whose keys are
//! the role local-parts and whose values are 64-char lowercase actor hex (the
//! canonical actor-id string form in this crate,
//! `segments::placement::actor_id_hex`); SQL NULL when no override is set.
//!
//! It is the shared, pure (`role-overrides`-gated for serde_json + hex) logic
//! behind the writer (`fauna.bridges.set_role_address`), the RCPT-time resolver
//! consumer (`bridge_routing_handlers::resolve_local_recipient`), and the
//! nest's projection onto the admin-mail wire, so all three agree on one
//! encoding — priority #2 (write the rule once, share it).

pub use fauna_protocol::bridge_routing::RoleAddressOverrides;

/// The role local-parts whose per-domain target is admin-overridable. The wire
/// `RoleAddressKind` enum (`fauna-protocol`) and the JSON storage keys both use
/// exactly these strings.
pub const OVERRIDABLE_ROLE_KEYS: [&str; 4] = ["postmaster", "abuse", "noc", "security"];

/// Parse the stored column. `None` / empty / malformed JSON → an empty map
/// (every role falls back to admin) — this never errors, holding the
/// never-reject invariant even on a corrupt column. A key this build does not
/// name is kept in the map's catch-all, never honoured by `resolve`, and
/// written back unchanged by [`to_stored`].
pub fn parse_stored(stored: Option<&str>) -> RoleAddressOverrides {
    match stored {
        None => RoleAddressOverrides::default(),
        Some(s) if s.trim().is_empty() => RoleAddressOverrides::default(),
        Some(s) => serde_json::from_str(s).unwrap_or_default(),
    }
}

/// Serialize for storage. `None` when no role carries an override, so the
/// caller writes SQL NULL (the canonical "no overrides" state) rather than `{}`.
pub fn to_stored(overrides: &RoleAddressOverrides) -> Option<String> {
    if overrides.is_empty() {
        None
    } else {
        // A flat object of string values — serialization cannot fail.
        serde_json::to_string(overrides).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: [u8; 32] = [0xABu8; 32];
    const ACTOR_B: [u8; 32] = [0x11u8; 32];

    fn hex_of(a: &[u8; 32]) -> String {
        hex::encode(a)
    }

    #[test]
    fn parse_none_and_empty_yield_empty_map() {
        assert!(parse_stored(None).is_empty());
        assert!(parse_stored(Some("")).is_empty());
        assert!(parse_stored(Some("   ")).is_empty());
    }

    #[test]
    fn parse_malformed_json_degrades_to_empty() {
        // A corrupt column must never panic or error — every role falls back to admin.
        assert!(parse_stored(Some("{not json")).is_empty());
        assert!(parse_stored(Some("[]")).is_empty());
    }

    #[test]
    fn parse_keeps_known_keys_and_never_honours_unknown_ones() {
        // tlsrpt / dmarc_report (non-overridable) or any junk key never route,
        // and a key this build does not name is written back unchanged.
        let json = format!(
            r#"{{"postmaster":"{}","tlsrpt":"{}","whatever":"x"}}"#,
            hex_of(&ACTOR_A),
            hex_of(&ACTOR_B)
        );
        let o = parse_stored(Some(&json));
        assert_eq!(o.postmaster.as_deref(), Some(hex_of(&ACTOR_A).as_str()));
        assert!(o.abuse.is_none());
        assert_eq!(o.resolve("tlsrpt"), None);
        let back = parse_stored(to_stored(&o).as_deref());
        assert_eq!(back, o, "the unnamed keys survive a rewrite");
    }

    #[test]
    fn resolve_hit_decodes_to_actor() {
        let mut o = RoleAddressOverrides::default();
        o.set("abuse", Some(hex_of(&ACTOR_A)));
        assert_eq!(o.resolve("abuse"), Some(ACTOR_A));
    }

    #[test]
    fn resolve_unset_key_falls_back_to_admin() {
        let o = RoleAddressOverrides::default();
        assert_eq!(o.resolve("postmaster"), None);
    }

    #[test]
    fn resolve_non_overridable_local_part_is_none() {
        let mut o = RoleAddressOverrides::default();
        // Even if a tlsrpt key were somehow present, resolve() never honors it.
        o.set("security", Some(hex_of(&ACTOR_A)));
        assert_eq!(o.resolve("tlsrpt"), None);
        assert_eq!(o.resolve("dmarc-report"), None);
        assert_eq!(o.resolve("bob"), None);
    }

    #[test]
    fn resolve_malformed_hex_degrades_to_none() {
        let mut o = RoleAddressOverrides::default();
        o.set("noc", Some("not-hex".into())); // wrong length + non-hex
        assert_eq!(o.resolve("noc"), None);
        o.set("noc", Some("ab".repeat(31))); // 62 chars — wrong length
        assert_eq!(o.resolve("noc"), None);
        o.set("noc", Some("zz".repeat(32))); // 64 chars but non-hex
        assert_eq!(o.resolve("noc"), None);
    }

    #[test]
    fn set_clear_removes_only_that_role() {
        let mut o = RoleAddressOverrides::default();
        o.set("postmaster", Some(hex_of(&ACTOR_A)));
        o.set("abuse", Some(hex_of(&ACTOR_B)));
        o.set("postmaster", None); // clear only postmaster
        assert!(o.postmaster.is_none());
        assert_eq!(o.resolve("abuse"), Some(ACTOR_B));
    }

    #[test]
    fn to_storage_is_none_when_empty_else_round_trips() {
        let mut o = RoleAddressOverrides::default();
        assert_eq!(to_stored(&o), None);

        o.set("postmaster", Some(hex_of(&ACTOR_A)));
        let json = to_stored(&o).expect("non-empty serializes");
        let back = parse_stored(Some(&json));
        assert_eq!(back, o);

        // Clearing the last key empties the map → storage NULL again.
        o.set("postmaster", None);
        assert_eq!(to_stored(&o), None);
    }

    #[test]
    fn overridable_keys_match_struct_fields() {
        // Guard: the constant and the resolve()/set() arms must stay in lockstep.
        let mut o = RoleAddressOverrides::default();
        for key in OVERRIDABLE_ROLE_KEYS {
            o.set(key, Some(hex_of(&ACTOR_A)));
            assert_eq!(o.resolve(key), Some(ACTOR_A), "key {key} must round-trip");
            o.set(key, None);
        }
    }
}

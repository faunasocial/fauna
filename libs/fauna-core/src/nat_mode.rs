//! Deployment NAT mode — the network-reachability axis (`public` / `private`).
//! Seeded pre-claim by the deployment artifact (`FAUNA_MODE`), confirmed by the
//! admin on the `nat_mode_choice` wizard page, and changeable any time from the
//! admin panel (`docs/goal/behavior/onboarding.md` § 3b-bis;
//! `docs/goal/architecture/nest/deployment-home-with-public-relay.md`). Unlike
//! the retired storage mode this axis is **mutable** — a public↔private flip
//! touches no at-rest data.
//!
//! One enum, one home: the nest (`bins/fauna-nest` re-exports it as
//! `config::NodeMode`), the onboarding machine's `NatModeSnapshot`, and the
//! admin client all share this type. The serde repr is `snake_case`
//! (`"public"` / `"private"`) — the same lowercase string used by the nest
//! config TOML, the `fauna.setup.nat_mode` request `mode` field, and
//! `fauna.setup.status`'s `node_mode` report.

use serde::{Deserialize, Serialize};

/// The nest's NAT mode: is the box internet-reachable?
///
/// - `Public` — internet-facing (typical VPS): serves federation/MX/MUA
///   endpoints on a public domain, obtains ACME certificates, relay side of
///   pairing.
/// - `Private` — behind NAT (home box): no MTA, no ACME, LAN-only IMAP/CalDAV
///   binds, pairs with a public nest as its relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum NodeMode {
    Public,
    Private,
}

impl NodeMode {
    /// Lowercase wire/string form (`"public"` / `"private"`) — matches the
    /// `snake_case` serde repr. Used in the `fauna.setup.nat_mode` request,
    /// the `fauna.setup.status` `node_mode` report, the nest config TOML, and
    /// `fauna.bridges.whoami`'s `node_mode` (so the MDA can pick a LAN-only
    /// bind default on the private axis).
    pub fn as_str(self) -> &'static str {
        match self {
            NodeMode::Public => "public",
            NodeMode::Private => "private",
        }
    }

    /// Parse the lowercase wire form. Strict — only `"public"` / `"private"`
    /// (no trimming, no case folding).
    pub fn from_wire_str(s: &str) -> Option<NodeMode> {
        match s {
            "public" => Some(NodeMode::Public),
            "private" => Some(NodeMode::Private),
            _ => None,
        }
    }
}

impl core::fmt::Display for NodeMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_str_round_trips() {
        assert_eq!(NodeMode::Public.as_str(), "public");
        assert_eq!(NodeMode::Private.as_str(), "private");
        assert_eq!(NodeMode::from_wire_str("public"), Some(NodeMode::Public));
        assert_eq!(NodeMode::from_wire_str("private"), Some(NodeMode::Private));
        assert_eq!(NodeMode::from_wire_str("Public"), None);
        assert_eq!(NodeMode::from_wire_str(" public"), None);
        assert_eq!(NodeMode::from_wire_str(""), None);
    }

    #[test]
    fn serde_repr_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&NodeMode::Public).unwrap(),
            "\"public\""
        );
        assert_eq!(
            serde_json::from_str::<NodeMode>("\"private\"").unwrap(),
            NodeMode::Private
        );
    }
}

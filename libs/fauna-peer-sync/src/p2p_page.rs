//! The linux P2P tab's possibility predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rules 2, 6 and 7; the page's ruling is
//! `docs/goal/behavior/p2p.md` § Element IDs → *Only what can work*).
//!
//! The page's service object (`P2pService`) is the linux app's own, so each
//! predicate takes the facts it reads as inputs and answers [`Possibility`]:
//! the tab paints every control from the answer, and each click asks the same
//! function first and refuses with the same reason. The failure reasons the
//! tab writes on its `error-message` live here too, so the wording of a
//! refusal and of a failure is decided once.

use std::net::Ipv4Addr;

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

/// The reason every control reading the service gives when the device's
/// peer service never started (its start failed at sign-in).
pub fn service_unavailable_reason() -> LocalizedText {
    LocalizedText::key("settings.p2p_page.service_unavailable")
}

/// `p2p-tunnel-toggle`: Start/Stop needs the device's peer service. Without
/// one the toggle is shown disabled with the reason, never a click that does
/// nothing (blocked, not absent: restarting the app may start the service).
pub fn tunnel_toggle_possibility(service_available: bool) -> Possibility {
    if service_available {
        Possibility::Possible
    } else {
        Possibility::blocked(service_unavailable_reason())
    }
}

/// `p2p-node-id-copy-btn`: there is a node id to copy only while the node
/// runs (`node_id` is what the service reports, `None` while stopped).
pub fn node_id_copy_possibility(node_id: Option<&str>) -> Possibility {
    match node_id {
        Some(id) if !id.trim().is_empty() => Possibility::Possible,
        _ => Possibility::blocked(LocalizedText::key(
            "settings.p2p_page.node_id_copy_needs_tunnel",
        )),
    }
}

/// `p2p-lan-copy-btn`: there is something to copy only when discovery found
/// at least one LAN address ([`crate::lan::discover_lan_candidates`]).
pub fn lan_copy_possibility(addrs: &[Ipv4Addr]) -> Possibility {
    if addrs.is_empty() {
        Possibility::blocked(LocalizedText::key("settings.p2p_page.lan_copy_none"))
    } else {
        Possibility::Possible
    }
}

/// `p2p-contact-remove-button`: removal needs the peer service (the contact
/// list is only painted while it runs, so this guards a service lost after
/// the paint).
pub fn contact_remove_possibility(service_available: bool) -> Possibility {
    tunnel_toggle_possibility(service_available)
}

/// The `error-message` text for a removal the store refused.
pub fn contact_remove_failed(name: &str, error: &str) -> LocalizedText {
    LocalizedText::key_args(
        "settings.p2p_page.contact_remove_failed",
        [("name", name), ("error", error)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(key: &str) -> Possibility {
        Possibility::blocked(LocalizedText::key(key))
    }

    #[test]
    fn the_toggle_needs_the_service() {
        assert_eq!(tunnel_toggle_possibility(true), Possibility::Possible);
        assert_eq!(
            tunnel_toggle_possibility(false),
            blocked("settings.p2p_page.service_unavailable")
        );
    }

    #[test]
    fn the_node_id_copies_only_while_the_node_runs() {
        assert_eq!(
            node_id_copy_possibility(Some("abc123")),
            Possibility::Possible
        );
        let stopped = blocked("settings.p2p_page.node_id_copy_needs_tunnel");
        assert_eq!(node_id_copy_possibility(None), stopped);
        assert_eq!(node_id_copy_possibility(Some("  ")), stopped);
    }

    #[test]
    fn the_lan_copy_needs_an_address() {
        assert_eq!(
            lan_copy_possibility(&[]),
            blocked("settings.p2p_page.lan_copy_none")
        );
        assert_eq!(
            lan_copy_possibility(&[Ipv4Addr::new(192, 168, 1, 7)]),
            Possibility::Possible
        );
    }

    #[test]
    fn contact_remove_needs_the_service_and_names_its_failure() {
        assert_eq!(contact_remove_possibility(true), Possibility::Possible);
        assert_eq!(
            contact_remove_possibility(false),
            blocked("settings.p2p_page.service_unavailable")
        );
        let failed = contact_remove_failed("Ana", "database is locked");
        assert_eq!(failed.key, "settings.p2p_page.contact_remove_failed");
        assert_eq!(failed.args["name"], "Ana");
        assert_eq!(failed.args["error"], "database is locked");
    }
}

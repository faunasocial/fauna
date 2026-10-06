//! tui's leg of the **offline share-initiation ceremony** — the co-present,
//! nest-free hand-off of a recipient set (`docs/goal/behavior/p2p.md`
//! § Offline share initiation).
//!
//! # What is left here, and why only this
//!
//! The ceremony itself — every ordering rule, every write-through, every
//! record and the folders page's own read — lives once, shared, in
//! [`fauna_sync_engine::offline_share`], and is re-exported below so this
//! app's call sites read unchanged. Two things genuinely cannot follow it
//! there, and they are the whole of this file:
//!
//! 1. **This app's device label.** The bind doors themselves are shared now
//!    (2026-08-23) — what a leg supplies is the string it calls itself by,
//!    and the actor-keyed `fauna_iroh` transport factory (`fauna_iroh::
//!    ceremony_transport`, shared with `apps/fauna-linux` since round 33 —
//!    the shared crate names no concrete transport substrate itself, the
//!    iroh-cleanliness bargain `fauna_sync_engine::peer_leg` documents, but
//!    both native apps assemble it identically). The wrapper below is that
//!    label applied to the shared doors, and nothing else.
//! 2. **The i18n *resolution*.** Only the resolution: which key each state
//!    and each refusal carries is a fact, and it lives once beside the types
//!    in [`fauna_client_capabilities::group_ceremony_view`]
//!    (`status_label` / `code_error_label`) — the arrangement `p2p.md`
//!    § Cross-user shared-set transfer already ratifies for this page's other
//!    readings, and that `share_glue.rs`'s `serve_status_text` next door has
//!    followed since the plane driver's own lift. The two doors below turn
//!    that key into a sentence through this app's own lookup, and do nothing
//!    else.
//!
//! Why the transport is actor-keyed and relay-less is the shared factory
//! type's own documentation ([`CeremonyTransportFactory`]) — this app only
//! satisfies it.
//!
//! The OTHER bind door is `crate::share_glue`: an account with ≥1 joined
//! cross-user set binds the same one endpoint from store-ready — via
//! [`bind_share_plane_seat`], with rule 7's cached brake read as the offline
//! evidence. Both doors take the session's [`SessionSeat`], so whichever asks
//! first binds and the other is handed that seat back.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_capabilities::group_ceremony_view::{
    CeremonyStatus, PeerCodeError, code_error_label, status_label,
};

pub use fauna_sync_engine::offline_share::*;

/// This app's name for itself in a ceremony — the string the person across
/// the table reads on their own screen while comparing codes.
const DEVICE_LABEL: &str = "fauna-tui";

/// This session's ceremony seat for the folders page panel — bound, or handed
/// back when the share plane already bound it. This app's label and transport
/// over the shared door. The bind never waits for the account runtime: the
/// runtime lends the ceremony record to the seat at its store-ready edge
/// (`SessionSeat::lend_record`).
pub async fn bind_seat(
    session_seat: &SessionSeat,
    nest: Arc<NestClient>,
    secret_hex: String,
) -> Result<Arc<CeremonySeat>, String> {
    fauna_sync_engine::offline_share::bind_seat(
        session_seat,
        nest,
        &secret_hex,
        DEVICE_LABEL,
        &fauna_iroh::ceremony_transport(),
    )
    .await
}

/// The share plane's bind door (`crate::share_glue`, row 58) — the SAME
/// session seat as [`bind_seat`], lent the transfer plane's roster. This
/// app's label and transport over the shared door.
pub(crate) async fn bind_share_plane_seat(
    session_seat: &SessionSeat,
    secret_hex: &str,
    evidence: Option<Vec<String>>,
    membership: &Arc<dyn fauna_peer_share::SetMembership + Send + Sync>,
    group_roster: &Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>,
) -> Result<(Arc<CeremonySeat>, Vec<std::net::SocketAddr>), String> {
    fauna_sync_engine::offline_share::bind_share_plane_seat(
        session_seat,
        secret_hex,
        DEVICE_LABEL,
        evidence,
        membership,
        group_roster,
        &fauna_iroh::ceremony_transport(),
    )
    .await
}

/// The refusal reading for a typed compare code, resolved through this app's
/// own i18n lookup. The key — and the judgement that an empty box says
/// nothing — are the shared crate's.
pub fn code_error_text(e: PeerCodeError) -> Option<String> {
    code_error_label(e).map(|t| crate::wizard::localized(&t))
}

/// The `offline-share-status` reading for a state.
pub fn status_text(status: CeremonyStatus) -> String {
    crate::wizard::localized(&status_label(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ceremony state resolves to its own English reading through tui's
    /// own lookup — the shared label's keys are in the table this app links,
    /// not just linux's. (The key-level uniqueness is pinned once, on the
    /// shared side.)
    #[test]
    fn every_ceremony_status_has_its_own_reading() {
        let all = [
            CeremonyStatus::Idle,
            CeremonyStatus::Expecting,
            CeremonyStatus::OfferSent,
            CeremonyStatus::AwaitingConsent,
            CeremonyStatus::Delivering,
            CeremonyStatus::Delivered,
            CeremonyStatus::Admitted,
            CeremonyStatus::Failed,
        ];
        let texts: Vec<String> = all.iter().map(|s| status_text(*s)).collect();
        for t in &texts {
            assert!(!t.is_empty(), "every status has a reading");
            assert!(
                !t.contains("folders.offline_share_status"),
                "a raw i18n key reached the surface: {t}"
            );
        }
        let unique: std::collections::BTreeSet<&String> = texts.iter().collect();
        assert_eq!(
            unique.len(),
            texts.len(),
            "two states sharing a reading would make the status element lie"
        );
    }

    #[test]
    fn an_empty_code_is_silent_but_a_wrong_one_speaks() {
        assert!(code_error_text(PeerCodeError::Empty).is_none());
        for e in [PeerCodeError::Malformed, PeerCodeError::OwnCode] {
            let t = code_error_text(e).expect("a wrong code speaks");
            assert!(
                !t.contains("folders.offline_share_code"),
                "a raw i18n key reached the surface: {t}"
            );
        }
    }
}

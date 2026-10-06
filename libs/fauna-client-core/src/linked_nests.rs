//! The nests an identity is linked to, as its pairing rows name them, and the
//! connection a client opens to one of them — the shape both of their readers
//! share: the account runtime's secondary leg (`fauna_account_plane::linked_leg`,
//! which re-exports these) and the seed-alone replacement's fan-out
//! (`fauna_client_recovery::linked_fanout`), so the "which pairing rows count"
//! rule is written once (`identity-succession.md` § Enforcement on the home
//! nest → *Every nest the identity is linked to*; `account-sync-plane.md`
//! § The bind leg).

use fauna_protocol::pair::{PairListReply, capability};

/// The WS-RPC kind the account's pairings are listed with, on the bound
/// nest's owner session.
pub const KIND_PAIR_LIST: &str = "fauna.pair.list";

/// A linked nest: the pairing row's nest id and the address it was linked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedNestTarget {
    /// The linked nest's Ed25519 identity — the pairing row's
    /// `private_nest_id`, and the identity its connection must be bound to.
    pub nest_id: [u8; 32],
    /// Where the nest was linked (`PairingRow::nest_url`).
    pub nest_url: String,
    /// The pairing carries [`capability::ACCOUNT_REPLICA`]: the secondary leg
    /// completes this nest. Without it the leg carries only the RecoveryKey
    /// registration chain there, which every addressed pairing gets whatever
    /// its capabilities.
    pub replica: bool,
}

/// A connection to a linked nest, as the host opened it: the requester and
/// the identity **the connection** is bound to — never the nest's own
/// `fauna.nest.info` claim (`security.md` § Transport trust, the
/// connection-bound identity rule: the pin graduated for that origin, else a
/// possession proof over this very connection). A caller compares it with
/// [`LinkedNestTarget::nest_id`] before it sends anything.
pub struct LinkedConnection<R> {
    pub rpc: R,
    pub bound_identity: [u8; 32],
}

/// The linked nests a pairing list names: every row with an address that is
/// not a nest in `bound` (the nest(s) the caller is already bound to), each
/// named once, in the list's order. [`LinkedNestTarget::replica`] is set when
/// any of a nest's rows carries [`capability::ACCOUNT_REPLICA`].
#[must_use]
pub fn linked_targets(reply: &PairListReply, bound: &[[u8; 32]]) -> Vec<LinkedNestTarget> {
    let mut targets: Vec<LinkedNestTarget> = Vec::new();
    for row in &reply.pairings {
        let Ok(nest_id) = <[u8; 32]>::try_from(row.private_nest_id.as_slice()) else {
            continue;
        };
        let Some(nest_url) = row.nest_url.clone().filter(|u| !u.trim().is_empty()) else {
            continue;
        };
        if bound.contains(&nest_id) {
            continue;
        }
        let replica = row
            .capabilities
            .iter()
            .any(|c| c == capability::ACCOUNT_REPLICA);
        match targets.iter_mut().find(|t| t.nest_id == nest_id) {
            Some(named) => named.replica |= replica,
            None => targets.push(LinkedNestTarget {
                nest_id,
                nest_url,
                replica,
            }),
        }
    }
    targets
}

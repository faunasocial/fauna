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

/// The domain the pairing mark's digest is derived under.
const PAIRING_MARK_CONTEXT: &str = "fauna pairing-mark v1";

/// The pairing mark's value for a nest's listing (`account-runtime.md`
/// § Multi-instance concurrency → *the pairing mark*, part 1): a digest of
/// the rows in nest-id order, each row's painted fields — nest id,
/// capabilities, expiry, label, address — and nothing else. The forward
/// queue riding the same reply is left out (it moves with no pairing
/// changing), and so is a row's `created_at`, which no page paints. A
/// function of the nest's answer alone, so two devices reading one listing
/// write one value. Both writers call it: the shared `LinkedNestsMachine`
/// after a gesture's re-list, and the account runtime's secondary leg.
#[must_use]
pub fn pairing_mark_digest(reply: &PairListReply) -> [u8; 32] {
    fn field(h: &mut blake3::Hasher, bytes: &[u8]) {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    fn optional(h: &mut blake3::Hasher, bytes: Option<&[u8]>) {
        match bytes {
            None => {
                h.update(&[0]);
            }
            Some(b) => {
                h.update(&[1]);
                field(h, b);
            }
        }
    }
    let row_digest = |row: &fauna_protocol::pair::PairingRow| {
        let mut h = blake3::Hasher::new_derive_key(PAIRING_MARK_CONTEXT);
        field(&mut h, &row.private_nest_id);
        h.update(&(row.capabilities.len() as u64).to_le_bytes());
        for c in &row.capabilities {
            field(&mut h, c.as_bytes());
        }
        optional(
            &mut h,
            row.expires_at
                .map(i64::to_le_bytes)
                .as_ref()
                .map(|b| &b[..]),
        );
        optional(&mut h, row.label.as_deref().map(str::as_bytes));
        optional(&mut h, row.nest_url.as_deref().map(str::as_bytes));
        (row.private_nest_id.to_vec(), *h.finalize().as_bytes())
    };
    let mut rows: Vec<(Vec<u8>, [u8; 32])> = reply.pairings.iter().map(row_digest).collect();
    // Nest-id order; two rows naming one nest order by their own digest, so
    // the nest's order never moves the value.
    rows.sort();
    let mut h = blake3::Hasher::new_derive_key(PAIRING_MARK_CONTEXT);
    h.update(&(rows.len() as u64).to_le_bytes());
    for (_, d) in &rows {
        h.update(d);
    }
    *h.finalize().as_bytes()
}

/// The pairing mark's key for the nest whose listing it speaks for: the
/// lowercase hex64 of the nest's proven id.
#[must_use]
pub fn pairing_mark_key(nest_id: &[u8; 32]) -> String {
    fauna_core::hex32::encode(nest_id)
}

#[cfg(test)]
mod pairing_mark_tests {
    use super::*;
    use fauna_protocol::pair::{ForwardQueue, PairingRow};

    fn row(id: u8) -> PairingRow {
        PairingRow {
            private_nest_id: vec![id; 32].into(),
            capabilities: vec!["account_replica".into(), "mail_pull".into()],
            expires_at: Some(1_900_000_000),
            created_at: 1_700_000_000,
            label: Some("home".into()),
            nest_url: Some("https://home.example".into()),
            extra: Default::default(),
        }
    }

    fn reply(rows: Vec<PairingRow>) -> PairListReply {
        PairListReply {
            pairings: rows,
            forward_queue: ForwardQueue::default(),
            extra: Default::default(),
        }
    }

    #[test]
    fn row_order_the_forward_queue_and_created_at_do_not_move_the_digest() {
        let base = pairing_mark_digest(&reply(vec![row(1), row(2)]));
        assert_eq!(base, pairing_mark_digest(&reply(vec![row(2), row(1)])));
        let mut queued = reply(vec![row(1), row(2)]);
        queued.forward_queue.queued = 9;
        queued.forward_queue.stuck = 2;
        queued.forward_queue.last_error = Some("refused".into());
        assert_eq!(base, pairing_mark_digest(&queued));
        let mut later = row(1);
        later.created_at += 50;
        assert_eq!(base, pairing_mark_digest(&reply(vec![later, row(2)])));
    }

    #[test]
    fn each_painted_field_moves_the_digest() {
        let base = pairing_mark_digest(&reply(vec![row(1)]));
        let mut edits: Vec<PairingRow> = Vec::new();
        let mut r = row(1);
        r.private_nest_id = vec![3; 32].into();
        edits.push(r);
        let mut r = row(1);
        r.capabilities.pop();
        edits.push(r);
        let mut r = row(1);
        r.expires_at = None;
        edits.push(r);
        let mut r = row(1);
        r.label = Some("relay".into());
        edits.push(r);
        let mut r = row(1);
        r.label = None;
        edits.push(r);
        let mut r = row(1);
        r.nest_url = Some("https://relay.example".into());
        edits.push(r);
        for edited in edits {
            assert_ne!(
                base,
                pairing_mark_digest(&reply(vec![edited.clone()])),
                "{edited:?}"
            );
        }
        assert_ne!(base, pairing_mark_digest(&reply(vec![])), "an unlink");
        assert_ne!(
            base,
            pairing_mark_digest(&reply(vec![row(1), row(2)])),
            "a link"
        );
    }
}

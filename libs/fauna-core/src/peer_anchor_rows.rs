//! The `fauna.state.peer-anchors` plane rows — the key grammar, the strict
//! value decode and the per-row join of the succession witness's peer-anchor
//! cache (`config-dissolution.md` owns
//! the kind's birth, plane-only, and § Phases and gates → *Bounded rows* the
//! row shape; `identity-succession.md` § The succession statement → *the
//! peer-profile harvest* owns what the anchors are; [`PeerAnchors::merge`]
//! owns the shipped rule).
//!
//! **One row per anchored actor per vector, never one per account.** The two
//! vectors hold up to [`MAX_PEER_ANCHOR_ENTRIES`] entries each, and at that
//! ceiling the pair is ~89 KiB before CBOR overhead — over the 64 KiB
//! per-entry cap the writer door enforces, so a `self` row would be bounded by
//! use, not by shape. Key `head/<actor hex64>` holds one [`PeerChainHead`],
//! key `domain/<actor hex64>` one [`PeerAnchorDomain`]; the value must name
//! its key's actor. A head's RecoveryKey is 32 bytes and a domain is a
//! non-empty host of at most [`crate::web::MAX_HOSTNAME_BYTES`] — the
//! writers' own bounds, moved to the door — so every row is bounded by its
//! own shape.
//!
//! The per-row join is the per-actor half of the shipped rule
//! ([`PeerChainHead::join_from`], [`PeerAnchorDomain::join_from`]).
//! **The ceiling moves to READ:** [`PeerAnchors::fold_row`] folds each row
//! through [`PeerAnchors::merge`], whose oldest-first truncation under
//! `MAX_PEER_ANCHOR_ENTRIES` by `peer_anchor_order` is associative (its
//! doc has the argument), so the fold over any set of rows, in any order,
//! reads exactly as [`PeerAnchors::merge`] of the replicas that wrote them. A
//! row the ceiling cuts stays stored and invisible — a CRDT kind has no
//! deletion — and the writer door (`fauna_account_plane::peer_anchor_rows`)
//! puts only the rows the fold keeps, so a replica never adds a row it
//! could not read back.
//!
//! **Decode posture — strict on the plane.** The kind is
//! CrdtPerField, so a tolerant reader would strip a newer build's field from
//! the bytes it re-encodes (`config-dissolution.md` P4). The two value types
//! stay tolerant (a `deny_unknown_fields` there would make an older build
//! refuse a newer build's record), so the plane gets its strictness the way the ledger's shared types do: a
//! row must re-encode to exactly its own bytes. A newer field is refused,
//! never stripped.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::data::{PeerAnchorDomain, PeerAnchors, PeerChainHead};
use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::{Error, Result};

/// The key prefix of a chain-head row.
pub const HEAD_PREFIX: &str = "head/";
/// The key prefix of a harvested-domain row.
pub const DOMAIN_PREFIX: &str = "domain/";

/// One `fauna.state.peer-anchors` row — which one the key's first segment
/// says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerAnchorRow {
    /// `head/<actor hex64>`.
    Head(PeerChainHead),
    /// `domain/<actor hex64>`.
    Domain(PeerAnchorDomain),
}

impl PeerAnchorRow {
    /// The row's plane key.
    #[must_use]
    pub fn plane_key(&self) -> String {
        match self {
            Self::Head(h) => format!("{HEAD_PREFIX}{}", crate::hex32::encode(&h.actor.0)),
            Self::Domain(d) => format!("{DOMAIN_PREFIX}{}", crate::hex32::encode(&d.actor.0)),
        }
    }

    /// The canonical value bytes of this row.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode_row(&self) -> Result<Vec<u8>> {
        match self {
            Self::Head(h) => canonical_encode(h),
            Self::Domain(d) => canonical_encode(d),
        }
        .map(|b| b.to_vec())
    }

    /// The per-row join — the per-actor half of [`PeerAnchors::merge`].
    ///
    /// # Errors
    /// The two rows are of different families or about different actors.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Head(a), Self::Head(b)) if a.actor == b.actor => {
                let mut joined = a.clone();
                joined.join_from(b);
                Ok(Self::Head(joined))
            }
            (Self::Domain(a), Self::Domain(b)) if a.actor == b.actor => {
                let mut joined = a.clone();
                joined.join_from(b);
                Ok(Self::Domain(joined))
            }
            _ => Err(Error::Encoding(
                "peer-anchor rows of different families or actors".into(),
            )),
        }
    }

    /// This one row as a single-entry [`PeerAnchors`].
    fn into_anchors(self) -> PeerAnchors {
        match self {
            Self::Head(h) => PeerAnchors {
                chain_heads: vec![h],
                anchor_domains: Vec::new(),
            },
            Self::Domain(d) => PeerAnchors {
                chain_heads: Vec::new(),
                anchor_domains: vec![d],
            },
        }
    }
}

/// Decode `value` as `T`, refusing anything that does not re-encode to the
/// same bytes (a non-canonical encoding, or a field this build drops).
fn decode_exact<T: Serialize + DeserializeOwned>(value: &[u8]) -> Result<T> {
    let decoded: T = canonical_decode(value)?;
    if canonical_encode(&decoded)?.as_slice() != value {
        return Err(Error::Encoding(
            "peer-anchor row does not re-encode to its own bytes".into(),
        ));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.peer-anchors` row: the key must be `head/` or
/// `domain/` and a lowercase hex64 actor id, the value must decode and
/// re-encode to its own bytes (the strict posture, module docs), name the
/// key's actor, and hold the writers' shape — a 32-byte RecoveryKey, or a
/// non-empty host within [`crate::web::MAX_HOSTNAME_BYTES`].
///
/// # Errors
/// An unparseable key, an undecodable or non-canonical value, an unknown
/// field, a value filed under another actor's key, or a value outside the
/// writers' shape.
pub fn decode_peer_anchor_row(key: &str, value: &[u8]) -> Result<PeerAnchorRow> {
    let row = if let Some(actor) = key.strip_prefix(HEAD_PREFIX) {
        if !crate::hex32::is_lowercase_hex64(actor) {
            return Err(Error::Encoding(format!("not a peer-anchors key: {key:?}")));
        }
        let head: PeerChainHead = decode_exact(value)?;
        if head.recovery_pubkey.len() != 32 {
            return Err(Error::Encoding(format!(
                "peer-anchor head {key:?} holds a {}-byte RecoveryKey",
                head.recovery_pubkey.len()
            )));
        }
        PeerAnchorRow::Head(head)
    } else if let Some(actor) = key.strip_prefix(DOMAIN_PREFIX) {
        if !crate::hex32::is_lowercase_hex64(actor) {
            return Err(Error::Encoding(format!("not a peer-anchors key: {key:?}")));
        }
        let domain: PeerAnchorDomain = decode_exact(value)?;
        if domain.domain.is_empty() || domain.domain.len() > crate::web::MAX_HOSTNAME_BYTES {
            return Err(Error::Encoding(format!(
                "peer-anchor domain {key:?} is empty or over the hostname bound"
            )));
        }
        PeerAnchorRow::Domain(domain)
    } else {
        return Err(Error::Encoding(format!("not a peer-anchors key: {key:?}")));
    };
    if row.plane_key() != key {
        return Err(Error::Encoding(format!(
            "peer-anchor row at {key:?} holds the entry for {:?}",
            row.plane_key()
        )));
    }
    Ok(row)
}

impl PeerAnchors {
    /// Every entry as its plane row, `(key, row)`, in key order, one row per
    /// (family, actor) — a vector holding two copies of one actor joins them.
    /// No ceiling here: the ceiling is the read fold's (module docs).
    #[must_use]
    pub fn rows(&self) -> Vec<(String, PeerAnchorRow)> {
        let mut rows = std::collections::BTreeMap::<String, PeerAnchorRow>::new();
        let all = self
            .chain_heads
            .iter()
            .cloned()
            .map(PeerAnchorRow::Head)
            .chain(
                self.anchor_domains
                    .iter()
                    .cloned()
                    .map(PeerAnchorRow::Domain),
            );
        for row in all {
            let key = row.plane_key();
            let joined = match rows.remove(&key) {
                // Same key ⇒ same family and actor, so the join cannot fail.
                Some(held) => held.merge(&row).unwrap_or(held),
                None => row,
            };
            rows.insert(key, joined);
        }
        rows.into_iter().collect()
    }

    /// Fold one plane row into `self` through the shipped rule — the read
    /// side of the per-actor rows, the ceiling included.
    ///
    /// # Errors
    /// Any refusal of [`decode_peer_anchor_row`].
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        let row = decode_peer_anchor_row(key, value)?;
        *self = self.merge(&row.into_anchors());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{MAX_PEER_ANCHOR_ENTRIES, Timestamp};
    use crate::identity::ActorId;

    fn head(actor: u8, key: u8, seq: u64, first_seen: u64, outrun: bool) -> PeerChainHead {
        PeerChainHead {
            actor: ActorId([actor; 32]),
            recovery_pubkey: vec![key; 32],
            seq,
            first_seen: Timestamp(first_seen),
            outrun,
        }
    }

    fn domain(actor: u8, host: &str, first_seen: u64) -> PeerAnchorDomain {
        PeerAnchorDomain {
            actor: ActorId([actor; 32]),
            domain: host.to_string(),
            first_seen: Timestamp(first_seen),
        }
    }

    fn fold(rows: &[(String, PeerAnchorRow)]) -> PeerAnchors {
        let mut out = PeerAnchors::default();
        for (k, r) in rows {
            out.fold_row(k, &r.encode_row().unwrap()).unwrap();
        }
        out
    }

    /// The fold is the shipped rule: two devices' anchors, split into rows
    /// and folded back, read exactly as
    /// [`PeerAnchors::merge`] of the two — an advance on one side, an
    /// equivocation at one height, an outrun mark on the shared head, two
    /// domains for one actor — in any row order.
    #[test]
    fn the_row_fold_reads_as_the_shipped_merge() {
        let a = PeerAnchors {
            chain_heads: vec![
                head(1, 0x10, 3, 50, false),
                head(2, 0x20, 1, 40, true),
                head(3, 0x30, 7, 90, false),
            ],
            anchor_domains: vec![domain(1, "b.example", 60), domain(4, "d.example", 10)],
        };
        let b = PeerAnchors {
            chain_heads: vec![
                head(1, 0x11, 5, 70, false),
                head(2, 0x20, 1, 30, false),
                head(3, 0x2f, 7, 95, false),
            ],
            anchor_domains: vec![domain(1, "a.example", 80)],
        };
        let mut rows = a.rows();
        rows.extend(b.rows());
        let folded = fold(&rows);
        assert_eq!(folded, a.merge(&b));
        rows.reverse();
        assert_eq!(fold(&rows), folded, "order-free");
        assert_eq!(
            fold(&folded.rows()),
            folded,
            "the fold's own rows fold back"
        );
    }

    /// **The ceiling at READ:** two replicas each holding a full vector of
    /// disjoint actors fold to the shipped merge's MAX_PEER_ANCHOR_ENTRIES
    /// oldest — however the rows interleave — and a cut row stays cut when
    /// its actor turns up again with a later sighting.
    #[test]
    fn the_fold_holds_the_ceiling_oldest_first() {
        let n = MAX_PEER_ANCHOR_ENTRIES;
        let mk = |base: u64, stamp: u64| PeerAnchors {
            chain_heads: (0..n as u64)
                .map(|i| PeerChainHead {
                    actor: ActorId({
                        let mut a = [0u8; 32];
                        a[..8].copy_from_slice(&(base + i).to_be_bytes());
                        a
                    }),
                    recovery_pubkey: vec![0x42; 32],
                    seq: 1,
                    first_seen: Timestamp(stamp + i * 2),
                    outrun: false,
                })
                .collect(),
            anchor_domains: Vec::new(),
        };
        let a = mk(0, 1_000);
        let b = mk(10_000, 1_001);
        let want = a.merge(&b);
        assert_eq!(want.chain_heads.len(), n);
        let mut rows = a.rows();
        rows.extend(b.rows());
        assert_eq!(fold(&rows), want);
        // Interleave by key order instead of replica order.
        rows.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(fold(&rows), want);
        // A cut actor re-harvested later cannot climb back in.
        let cut = b.chain_heads.last().unwrap().clone();
        assert!(!want.chain_heads.iter().any(|h| h.actor == cut.actor));
        let mut again = cut.clone();
        again.first_seen = Timestamp(u64::MAX / 2);
        rows.push((
            PeerAnchorRow::Head(again.clone()).plane_key(),
            PeerAnchorRow::Head(again),
        ));
        assert_eq!(fold(&rows), want);
    }

    /// A key outside the grammar, a misfiled value, a value outside the
    /// writers' shape, a cross-family or cross-actor join, and a field from a
    /// newer build are all refused.
    #[test]
    fn junk_misfiled_misshapen_or_newer_rows_are_refused() {
        let h = PeerAnchorRow::Head(head(1, 0x10, 3, 50, false));
        let d = PeerAnchorRow::Domain(domain(1, "a.example", 60));
        for row in [&h, &d] {
            let key = row.plane_key();
            let value = row.encode_row().unwrap();
            assert_eq!(decode_peer_anchor_row(&key, &value).unwrap(), *row);
            assert!(decode_peer_anchor_row("self", &value).is_err());
            assert!(decode_peer_anchor_row(&key.to_uppercase(), &value).is_err());
        }
        // A head filed as a domain, and under another actor.
        assert!(decode_peer_anchor_row(&d.plane_key(), &h.encode_row().unwrap()).is_err());
        let other = PeerAnchorRow::Head(head(2, 0x10, 3, 50, false));
        assert!(decode_peer_anchor_row(&other.plane_key(), &h.encode_row().unwrap()).is_err());
        assert!(h.merge(&other).is_err());
        assert!(h.merge(&d).is_err());
        // Outside the writers' shape.
        let mut short = head(1, 0x10, 3, 50, false);
        short.recovery_pubkey.pop();
        let short = PeerAnchorRow::Head(short);
        assert!(decode_peer_anchor_row(&short.plane_key(), &short.encode_row().unwrap()).is_err());
        for host in [
            String::new(),
            "h".repeat(crate::web::MAX_HOSTNAME_BYTES + 1),
        ] {
            let bad = PeerAnchorRow::Domain(domain(1, &host, 60));
            assert!(decode_peer_anchor_row(&bad.plane_key(), &bad.encode_row().unwrap()).is_err());
        }
        // A field from a newer build, and a non-canonical encoding.
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a PeerAnchorDomain,
            from_the_future: u8,
        }
        let PeerAnchorRow::Domain(inner) = &d else {
            unreachable!()
        };
        let newer = canonical_encode(&Newer {
            record: inner,
            from_the_future: 1,
        })
        .unwrap();
        assert!(decode_peer_anchor_row(&d.plane_key(), &newer).is_err());
        let canonical = d.encode_row().unwrap();
        let mut loose = vec![0xb8, canonical[0] & 0x1f];
        loose.extend_from_slice(&canonical[1..]);
        assert!(decode_peer_anchor_row(&d.plane_key(), &loose).is_err());
    }
}

//! The **ranged chunk pull** both peer legs share: assemble stored chunk bodies
//! that cross the channel one bounded slice at a time, and check each against
//! its store key before anything uses it.
//!
//! A chunk body is up to 8 MiB (`fauna_core::chunker::MAX_STORED_CHUNK_BODY`)
//! and a peer-channel frame caps at 1 MiB, so a body arrives as a run of
//! slices, each re-wanted from the length this side already holds. The round
//! trip itself is the caller's — the same-account leg sends
//! `fauna.peer.sync.chunks.pull`, the cross-user share leg
//! `fauna.peer.share.chunks.pull` — and this module owns the rest: which keys
//! are still wanted, what a slice may claim, when a body is whole, and the
//! content-address check on completion (wormability rule 4).
//!
//! The legs differ in one rule, chosen by [`Missing`]: a share pull holds one
//! member to a whole file, so a key the peer lacks fails it; a same-account
//! pull takes what a sibling has and leaves the rest to the nest.

use std::collections::HashMap;
use std::future::Future;

use anyhow::{Result, bail};
use fauna_core::chunker::MAX_STORED_CHUNK_BODY;
use fauna_core::data::ContentHash;

/// The most rounds one want list runs. A round moves up to one reply's budget
/// (~700 KiB) and a body caps at 8 MiB, so one body needs ~12 rounds and a
/// batch proportionally more; the cap only stops a peer that never advances,
/// which the no-progress refusal catches far sooner. A Rust constant — never a
/// knob.
pub const MAX_ROUNDS: usize = 512;

/// One reply's byte budget for served slices, under the 1 MiB frame
/// (`fauna_peer_channel::MAX_FRAME_LEN`) with room for the envelope.
pub const MAX_BODY_BYTES_PER_REPLY: usize = 700 * 1024;

/// What a key the peer reports missing does to the pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Fail the whole pull — the share leg: one member serves the whole file
    /// or none of it.
    Fails,
    /// Stop wanting it and report it — the same-account leg: the nest serves
    /// what a sibling lacks.
    Reported,
}

/// One served slice, as the caller's reply carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    /// The store key as it crossed the wire (checked to be 32 bytes here).
    pub store_key: Vec<u8>,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub total_len: u64,
}

/// One round's reply: the slices served and the keys the peer does not hold.
/// A deferred key is simply absent from both — it stays wanted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Round {
    pub slices: Vec<Slice>,
    pub missing: Vec<Vec<u8>>,
}

/// What a pull assembled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assembled {
    /// Whole, content-address-verified bodies by store key.
    pub bodies: HashMap<ContentHash, Vec<u8>>,
    /// Keys the peer reported missing ([`Missing::Reported`] only), in want
    /// order.
    pub missing: Vec<ContentHash>,
}

/// Assemble the bodies of `store_keys` over `round`, which sends one request
/// for the `(key, offset)` wants it is handed and returns the reply.
///
/// `arrived` counts every slice byte a reply carried, **before** the slice is
/// judged: bytes a refused slice brought still crossed the wire, and a caller
/// metering the transfer charges them. Distinct keys only — a key named twice
/// is pulled once.
///
/// Refused, never patched: a slice for a key that was not wanted, at an offset
/// other than where this side left off, declaring a length over the ceiling
/// for any stored chunk or different from what it declared before, or
/// overrunning its declared length; a round that moved nothing while bodies
/// are still wanted; and, on completion, a body that does not hash to its key.
pub async fn assemble<F, Fut>(
    store_keys: &[ContentHash],
    relative_path: &str,
    missing_policy: Missing,
    arrived: &mut u64,
    mut round: F,
) -> Result<Assembled>
where
    F: FnMut(Vec<(ContentHash, u64)>) -> Fut,
    Fut: Future<Output = Result<Round>>,
{
    struct Building {
        bytes: Vec<u8>,
        total_len: Option<u64>,
    }
    let mut building: HashMap<ContentHash, Building> = HashMap::new();
    let mut distinct: Vec<ContentHash> = Vec::new();
    for key in store_keys {
        if !distinct.contains(key) {
            distinct.push(*key);
            building.insert(
                *key,
                Building {
                    bytes: Vec::new(),
                    total_len: None,
                },
            );
        }
    }
    let mut missing: Vec<ContentHash> = Vec::new();

    // A body is whole when its assembled length reaches the served
    // `total_len`; until then it is re-wanted from that length.
    let outstanding = |building: &HashMap<ContentHash, Building>, missing: &[ContentHash]| {
        distinct
            .iter()
            .filter(|k| !missing.contains(k))
            .filter(|k| {
                let b = &building[k];
                b.total_len != Some(b.bytes.len() as u64)
            })
            .copied()
            .collect::<Vec<_>>()
    };

    for _round in 0..MAX_ROUNDS {
        let wanted = outstanding(&building, &missing);
        if wanted.is_empty() {
            break;
        }
        let reply = round(
            wanted
                .iter()
                .map(|k| (*k, building[k].bytes.len() as u64))
                .collect(),
        )
        .await?;
        // Counted before any slice is judged (see the doc).
        *arrived = reply
            .slices
            .iter()
            .fold(*arrived, |acc, s| acc.saturating_add(s.bytes.len() as u64));

        for gone in &reply.missing {
            if missing_policy == Missing::Fails {
                bail!(
                    "the peer does not hold chunk {} of {relative_path}",
                    hex::encode(gone)
                );
            }
            let key = key_of(gone)?;
            // A key never asked this round names nothing here.
            if wanted.contains(&key) && !missing.contains(&key) {
                missing.push(key);
            }
        }

        let mut advanced = 0usize;
        for slice in reply.slices {
            let key = key_of(&slice.store_key)?;
            let claimed = key.digest();
            let Some(slot) = building.get_mut(&key).filter(|_| !missing.contains(&key)) else {
                bail!(
                    "the peer served chunk {} of {relative_path}, which was never wanted",
                    hex::encode(claimed)
                );
            };
            // A slice must continue exactly where this side left off: a gap, a
            // rewind or an overlap would silently corrupt the body.
            if slice.offset != slot.bytes.len() as u64 {
                bail!(
                    "peer-served chunk {} of {relative_path} arrived at offset {} but this \
                     side holds {} bytes — a non-contiguous slice",
                    hex::encode(claimed),
                    slice.offset,
                    slot.bytes.len()
                );
            }
            // The declared length is the peer's word, and it decides how many
            // rounds this side keeps buffering for. No honest body is longer
            // than a sealed maximum-size chunk, so a longer claim is refused on
            // the slice that makes it — before a byte of it is kept. Without
            // this a hostile peer could declare an enormous length and feed one
            // reply's budget per round for `MAX_ROUNDS` rounds, hundreds of MiB
            // held here, then stall before the hash check at the end ever runs.
            if slice.total_len > MAX_STORED_CHUNK_BODY {
                bail!(
                    "peer-served chunk {} of {relative_path} declares a {}-byte body, over the \
                     {MAX_STORED_CHUNK_BODY}-byte ceiling for any stored chunk",
                    hex::encode(claimed),
                    slice.total_len
                );
            }
            match slot.total_len {
                Some(known) if known != slice.total_len => bail!(
                    "peer-served chunk {} of {relative_path} changed its total length \
                     mid-transfer ({known} then {})",
                    hex::encode(claimed),
                    slice.total_len
                ),
                Some(_) => {}
                None => slot.total_len = Some(slice.total_len),
            }
            if slot.bytes.len() as u64 + slice.bytes.len() as u64 > slice.total_len {
                bail!(
                    "peer-served chunk {} of {relative_path} overran its declared length",
                    hex::encode(claimed)
                );
            }
            advanced += slice.bytes.len();
            slot.bytes.extend_from_slice(&slice.bytes);
        }
        if advanced == 0 && outstanding(&building, &missing) == wanted {
            // Nothing moved, nothing newly whole and nothing newly missing: no
            // later round can do better, so say so instead of spinning to the
            // cap.
            bail!(
                "the peer served no bytes for the outstanding chunks of {relative_path} \
                 — no progress is possible"
            );
        }
    }
    let stalled = outstanding(&building, &missing);
    if !stalled.is_empty() {
        bail!(
            "{} chunk(s) of {relative_path} still incomplete after {MAX_ROUNDS} pull rounds",
            stalled.len()
        );
    }

    // Rule 4, on completion: the assembled body must hash to the key it was
    // served under. A slice has no address of its own, so this is the first
    // and only point the check means anything — and it runs before any body
    // leaves this function.
    let mut out = Assembled {
        bodies: HashMap::new(),
        missing,
    };
    for key in distinct {
        if out.missing.contains(&key) {
            continue;
        }
        let body = building
            .remove(&key)
            .expect("every distinct key is building")
            .bytes;
        let actual = ContentHash::of_raw(&body);
        if actual != key {
            bail!(
                "peer-served chunk hash mismatch in {relative_path}: served under {}, \
                 assembled bytes hash to {}",
                hex::encode(key.digest()),
                hex::encode(actual.digest())
            );
        }
        out.bodies.insert(key, body);
    }
    Ok(out)
}

/// The serve half: the slice of `body` a want at `offset` gets within
/// `budget` bytes. `None` for an offset past the body's end — a protocol
/// error the caller answers loudly (an empty slice would read as complete).
/// `offset == body.len()` yields the empty slice that ends an ordinary body.
pub fn slice_for(body: &[u8], offset: u64, budget: usize) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    if start > body.len() {
        return None;
    }
    let end = start.saturating_add(budget).min(body.len());
    Some(&body[start..end])
}

fn key_of(wire: &[u8]) -> Result<ContentHash> {
    let Ok(digest) = <[u8; 32]>::try_from(wire) else {
        bail!("a store key must be exactly 32 bytes");
    };
    Ok(ContentHash::from_digest_raw(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize, seed: u8) -> (ContentHash, Vec<u8>) {
        let bytes: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_add(seed)).collect();
        (ContentHash::of_raw(&bytes), bytes)
    }

    /// A peer holding `held`, serving `slice` bytes per key per round.
    fn serve(
        held: &HashMap<ContentHash, Vec<u8>>,
        wants: Vec<(ContentHash, u64)>,
        slice: usize,
    ) -> Round {
        let mut round = Round::default();
        for (key, offset) in wants {
            match held.get(&key) {
                None => round.missing.push(key.digest().to_vec()),
                Some(b) => round.slices.push(Slice {
                    store_key: key.digest().to_vec(),
                    offset,
                    bytes: slice_for(b, offset, slice).unwrap().to_vec(),
                    total_len: b.len() as u64,
                }),
            }
        }
        round
    }

    /// A body bigger than one slice arrives whole over several rounds, and
    /// every byte that crossed is counted.
    #[tokio::test]
    async fn a_body_assembles_across_rounds() {
        let (key, bytes) = body(10_000, 3);
        let held = HashMap::from([(key, bytes.clone())]);
        let mut arrived = 0;
        let out = assemble(&[key, key], "f", Missing::Fails, &mut arrived, |w| {
            let r = serve(&held, w, 3_000);
            async move { Ok(r) }
        })
        .await
        .unwrap();
        assert_eq!(out.bodies[&key], bytes);
        assert!(out.missing.is_empty());
        assert_eq!(arrived, 10_000);
    }

    /// The same-account rule: a key the sibling lacks is reported, never
    /// fatal, and the keys it holds still arrive.
    #[tokio::test]
    async fn a_reported_miss_leaves_the_rest_whole() {
        let (held_key, bytes) = body(5_000, 1);
        let (gone_key, _) = body(5_000, 2);
        let held = HashMap::from([(held_key, bytes.clone())]);
        let mut arrived = 0;
        let out = assemble(
            &[gone_key, held_key],
            "f",
            Missing::Reported,
            &mut arrived,
            |w| {
                let r = serve(&held, w, 2_000);
                async move { Ok(r) }
            },
        )
        .await
        .unwrap();
        assert_eq!(out.bodies.len(), 1);
        assert_eq!(out.bodies[&held_key], bytes);
        assert_eq!(out.missing, vec![gone_key]);
    }

    /// The share rule: one miss fails the pull.
    #[tokio::test]
    async fn a_miss_fails_the_share_pull() {
        let (gone_key, _) = body(5_000, 2);
        let mut arrived = 0;
        let err = assemble(&[gone_key], "f", Missing::Fails, &mut arrived, |w| {
            let r = serve(&HashMap::new(), w, 2_000);
            async move { Ok(r) }
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("does not hold chunk"), "{err:#}");
    }

    /// A body that does not hash to its key is refused on completion.
    #[tokio::test]
    async fn a_forged_body_is_refused() {
        let (key, _) = body(4_000, 1);
        let (_, forged) = body(4_000, 9);
        let held = HashMap::from([(key, forged)]);
        let mut arrived = 0;
        let err = assemble(&[key], "f", Missing::Reported, &mut arrived, |w| {
            let r = serve(&held, w, 1_000);
            async move { Ok(r) }
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("hash mismatch"), "{err:#}");
        assert_eq!(arrived, 4_000, "the forged bytes still crossed the wire");
    }

    /// A peer deferring everything forever is refused, not spun on.
    #[tokio::test]
    async fn a_peer_that_never_serves_is_refused() {
        let (key, _) = body(4_000, 1);
        let mut arrived = 0;
        let err = assemble(&[key], "f", Missing::Reported, &mut arrived, |_w| async {
            Ok(Round::default())
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no progress"), "{err:#}");
    }

    /// The serve half: an offset past the end is refused; the end itself is
    /// the empty slice.
    #[test]
    fn slice_for_bounds() {
        let b = [1u8, 2, 3, 4];
        assert_eq!(slice_for(&b, 1, 2), Some(&b[1..3]));
        assert_eq!(slice_for(&b, 3, 10), Some(&b[3..4]));
        assert_eq!(slice_for(&b, 4, 10), Some(&b[4..4]));
        assert_eq!(slice_for(&b, 5, 10), None);
    }
}

//! The peer-served change-row provenance ruling, as code — **both halves in
//! one module** so the serve filter and the ingest check can never drift apart.
//!
//! Owner: `docs/goal/behavior/p2p.md` § *Peer-served change-row provenance*
//! (ruled 2026-08-17; the relayed-row refusal lifted by
//! `writer-signed-change-records.md` ruling
//! (3)). The ruling in one line: **bytes are multi-source; a row travels as far
//! as its proof does.**
//!
//! Chunks and manifests are self-verifying (store-keyed by hash), so *who*
//! serves them carries no weight — nothing in this module touches them. A
//! change **row** carries one of two proofs:
//!
//! - **Its writer's signature** — the row plus its inline cert verify
//!   self-contained through the one shared reader
//!   ([`fauna_protocol::sync_row_verify::RowReader`]), so a signed row of ANY
//!   writer relays: a peer serves every row it holds, and the receiver judges
//!   each under its own set nonce and cached writer roster.
//! - **The channel's proof (PT-1b)** — an unsigned row's only evidence is that
//!   the connection's proven peer served it as its own. Every writer signs, so
//!   an unsigned row outside the signature check's class exemptions is refused
//!   whoever serves it; an exempt-class row is admitted only as the serving
//!   peer's own, only from a peer the cached roster records as a writer. A
//!   relayed unsigned row is refused either way: nobody can vouch for it.
//!
//! The halves:
//!
//! - **[`serves_held_row`] (serve side)** — the peer's own rows (own-pending
//!   and own-sequenced) and every signed row it relays; never an unsigned row
//!   of someone else's.
//! - **[`screen_peer_row`] (ingest, binding-free)** — what the crate-side pull
//!   can decide without the set's nonce: a signed row passes on to the reader;
//!   an unsigned one must pass the channel-proof rule.
//! - **[`judge_peer_row`] (ingest, authoritative)** — the reader's verdict over
//!   the row as served, over a reader the cached roster stands in for where it
//!   never read its own ([`reader_over_cached_roster`]). The state writer (the
//!   engine) runs this; it never
//!   trusts that the screen ran.
//!
//! Neither half trusts the other: the ingest checks are written as if the
//! serve filter did not exist, because a hostile peer runs no filter of ours.

use fauna_core::encoding::EmbedAsBytes;
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::{Held, RowReader, RowVerdict, WriterRoster};
use fauna_protocol::sync_writer_sig::ChangeVerifyError;

/// A change row as the serving replica holds it, with the facts the serve
/// filter needs. Produced by the [`ShareStore`](crate::server::ShareStore)
/// seam; [`Self::change`], [`Self::sequenced`] and [`Self::signer_cert`] cross
/// the wire.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalShareChange {
    pub change: SyncChange,
    /// `true` ⇒ a nest has sequenced this row into the set's log. `false` ⇒ it
    /// is still un-sequenced (a writer's offline-authored pending row).
    pub sequenced: bool,
    /// `true` ⇒ this replica's **own writer** produced the row. The store
    /// answers it from local origin (its own outbox / writer id), never from
    /// the row's contents — which is how it separates an own row from one it
    /// holds for relay, a question the row itself cannot answer.
    pub locally_authored: bool,
    /// The row's delegated signer cert, served inline beside it (`None` for a
    /// direct signer or an unsigned row).
    pub signer_cert: Option<EmbedAsBytes>,
}

/// Whether `row` carries a writer signature a receiver can judge — the pair
/// and the signed actor the statement names.
fn is_signed(row: &SyncChange) -> bool {
    row.signature.is_some() && row.signer_key.is_some() && row.author_actor_id.is_some()
}

/// The serve-side filter: may this replica serve `row` to an admitted member?
///
/// `own_actor_hex` is this node's actor id in the lowercase-hex spelling
/// [`SyncChange::author_actor_id`] uses.
///
/// - **An own row** serves signed or not: unsigned, the receiver's only proof
///   is the channel, which proves exactly this peer. Two signals must agree
///   where both exist — a sequenced row needs the author stamp to name us and
///   local origin to confirm it; an un-sequenced row with no stamp has local
///   origin alone (a pre-signing pending row).
/// - **A relayed row** serves only signed: its writer's signature is the one
///   proof that survives the hop, so an unsigned relay would be refused on
///   every receiver — serving it only launders a claim.
pub fn serves_held_row(row: &LocalShareChange, own_actor_hex: &str) -> bool {
    if !row.locally_authored {
        return is_signed(&row.change);
    }
    match row.change.author_actor_id.as_deref() {
        // A stamp that names someone else outranks the local claim: a store
        // answering `locally_authored` for a row attributed elsewhere is a
        // store bug, and serving it would launder a misattribution.
        Some(stamped) => stamped.eq_ignore_ascii_case(own_actor_hex),
        // No stamp. Legitimate only while un-sequenced (own-pending): a
        // SEQUENCED row that came back from a nest without an author stamp is a
        // pre-multi-writer row whose recorder this side cannot prove was
        // itself, so it is not served.
        None => !row.sequenced,
    }
}

/// Why an ingest refused a peer-served row. Each variant is a distinct
/// mechanism, so a caller can log the honest reason (and a test can name it)
/// instead of collapsing every refusal into one opaque failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowRefusal {
    /// The cached roster records no writer role for the row's writer on this
    /// set — the serving peer, for an unsigned row; the signed actor, for a
    /// signed one the reader's own roster has not judged yet. **The
    /// fail-closed arm**: no cached role ⇒ rows refused, bytes still
    /// served/pulled. A stale cache denies a fresh writer until the next online
    /// roster read (which heals it).
    NotACachedWriter,
    /// An UNSIGNED row that names an author other than the channel-proven
    /// serving peer — an unsigned relay, which nothing can vouch for.
    RelayedThirdPartyRow,
    /// An unsigned row with no author stamp that claims to be sequenced. Only
    /// an un-sequenced row may be stampless (own-pending); a sequenced
    /// stampless row is unattributable, and unattributable is not accepted.
    SequencedWithoutAuthor,
    /// The row's writer signature did not verify — a fabricated or altered
    /// row, one bound to another set's nonce, or an unsigned one.
    DidNotVerify(ChangeVerifyError),
    /// The row cannot be judged yet (no set nonce in custody): not
    /// accepted now, and a retry may accept it — the caller keeps its cursor
    /// below a held sequenced row.
    NotJudgeableYet(Held),
    /// The row verified under a retired nonce of the set's lineage, signed as
    /// a predecessor of the set's owner — history
    /// (`writer-signed-change-records.md` ruling (11)(c)): a version of the
    /// path, never live state, so never a provisional overlay either.
    History,
}

impl RowRefusal {
    /// A short, honest reason string — for a log line or an error's details.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotACachedWriter => {
                "the cached roster records no writer role for the row's writer on this set"
            }
            Self::RelayedThirdPartyRow => {
                "an unsigned row names an author other than the channel-proven serving peer"
            }
            Self::SequencedWithoutAuthor => {
                "a sequenced row carries no author stamp — unattributable"
            }
            Self::DidNotVerify(_) => "the row's writer signature did not verify",
            Self::NotJudgeableYet(_) => "the row cannot be judged yet (no set nonce in custody)",
            Self::History => "the row is history — a predecessor's version under a retired nonce",
        }
    }
}

/// What admitted a peer-served row — both enter the provisional overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRowAdmission {
    /// The row's writer signature verified, bound to this set, and its signed
    /// actor is a writer — whoever served it.
    Verified { writer: [u8; 32] },
    /// Admitted on the channel's proof alone: the serving peer's own row of a
    /// class the signature check does not apply to.
    ChannelProven,
}

/// The channel-proof rule — the only proof an unsigned row has: the serving
/// peer (`proven_actor_hex`, PT-1b) is a cached writer and the row is its own.
fn channel_proof(
    change: &SyncChange,
    sequenced: bool,
    proven_actor_hex: &str,
    peer_is_cached_writer: bool,
) -> Result<(), RowRefusal> {
    // The roster consult first: an unknown writer's rows are refused before
    // their contents are weighed at all.
    if !peer_is_cached_writer {
        return Err(RowRefusal::NotACachedWriter);
    }
    match change.author_actor_id.as_deref() {
        Some(stamped) if stamped.eq_ignore_ascii_case(proven_actor_hex) => Ok(()),
        Some(_) => Err(RowRefusal::RelayedThirdPartyRow),
        // Stampless: attributable to the proven peer only while un-sequenced.
        None if !sequenced => Ok(()),
        None => Err(RowRefusal::SequencedWithoutAuthor),
    }
}

/// The crate-side ingest screen: what the pull can decide with no set binding
/// (no nonce, no reader). A signed row passes on — only the reader can judge it
/// ([`judge_peer_row`]); an unsigned row must pass the channel-proof rule here,
/// so an unsigned relay never costs a body fetch.
///
/// `peer_is_cached_writer` is the receiver's **cached writer roster** answer for
/// the serving peer — `false` whenever the receiver has no cached role at all.
/// That default is the ruling's fail-closed arm, and it is a plain `bool`
/// precisely so a caller cannot express "unknown".
pub fn screen_peer_row(
    change: &SyncChange,
    sequenced: bool,
    proven_actor_hex: &str,
    peer_is_cached_writer: bool,
) -> Result<(), RowRefusal> {
    if is_signed(change) {
        return Ok(());
    }
    channel_proof(change, sequenced, proven_actor_hex, peer_is_cached_writer)
}

/// The authoritative ingest check: may the receiver admit `change` (marked
/// `sequenced` on the wire) served by the channel-proven peer
/// `proven_actor_hex`?
///
/// `reader` is the receiver's reader for this set **over the share leg's
/// cached roster** ([`reader_over_cached_roster`]) — its binding (nonce,
/// lineage, owner), a writer roster, and the row's inline cert already
/// ingested ([`RowReader::ingest_certs`]). `peer_is_cached_writer` is the
/// cached roster's answer for the serving peer, as [`screen_peer_row`] takes
/// it. Judged AS SERVED — before any receiver rewrite of a signed field.
///
/// - The reader's verdict decides a signed row, and it is the nest pull's
///   verdict (`writer-signed-change-records.md` ruling (11)(i): the same
///   judge over the same binding, nothing of the door's own): verified ⇒
///   admitted whoever served it; refused ⇒ refused; history ⇒ refused as
///   history; held ⇒ held.
/// - A reader with no roster at all — never read, and nothing cached to stand
///   in for it — places nobody but the owner: the row is refused, fail
///   closed. The serving peer's role is not asked.
/// - A row the reader exempts from the signature check is admitted only on
///   the channel's proof — the serving peer's own row, from a cached writer.
///   An unsigned row outside the exemptions is refused; a signed row read
///   with no set nonce in custody is held.
///
/// `Ok` means the row may be ingested **as a provisional read-side overlay** —
/// never into the nest-sequenced log store; a provisional *delete* never
/// destroys local bytes (the consumer's obligations, same goal-doc section).
pub fn judge_peer_row(
    reader: &RowReader,
    change: &SyncChange,
    sequenced: bool,
    proven_actor_hex: &str,
    peer_is_cached_writer: bool,
) -> Result<PeerRowAdmission, RowRefusal> {
    match reader.judge(change) {
        // The signed actor comes from the verdict, never from the served
        // stamp (`writer-signed-change-records.md` ruling (8)(c)).
        RowVerdict::Verified { signed_as, .. } => {
            Ok(PeerRowAdmission::Verified { writer: signed_as })
        }
        RowVerdict::Held(Held::RosterUnread) => Err(RowRefusal::NotACachedWriter),
        RowVerdict::Held(held) => Err(RowRefusal::NotJudgeableYet(held)),
        RowVerdict::Refused(e) => Err(RowRefusal::DidNotVerify(e)),
        RowVerdict::History { .. } => Err(RowRefusal::History),
        RowVerdict::Exempt => {
            channel_proof(change, sequenced, proven_actor_hex, peer_is_cached_writer)
                .map(|()| PeerRowAdmission::ChannelProven)
        }
    }
}

/// The receiver's reader for a peer-served page: `reader` as it stands, and —
/// where it never read its roster — the share leg's **cached** roster
/// installed in its place, the last roster that stands offline
/// (`writer-signed-change-records.md` rulings (3), (8)(e), (11)(c)/(i)).
///
/// `cached` carries each writer's proven predecessors beside the writer,
/// nearest first, so the one judge places a retired identity by its chain —
/// the owner's predecessor under a nonce its successor minted is refused, one
/// under a retired nonce is history — instead of meeting it as a writer row.
/// An empty cache installs nothing: no roster stands, and
/// [`judge_peer_row`] refuses every non-owner's row. A roster the reader read
/// itself is never replaced.
pub fn reader_over_cached_roster(mut reader: RowReader, cached: WriterRoster) -> RowReader {
    if !reader.roster_read() && !cached.writers.is_empty() {
        reader.install_roster(cached);
    }
    reader
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::sync_row_verify::ReaderBinding;
    use fauna_protocol::sync_writer_sig::ChangeSigner;

    const NONCE: [u8; 32] = [7; 32];

    fn kp(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    fn hex_of(k: &ActorKeypair) -> String {
        k.actor_id().to_hex()
    }

    fn row(author: Option<&str>) -> SyncChange {
        SyncChange {
            seq: 12,
            path_hash: "cc".repeat(32),
            manifest_hash: Some("dd".repeat(32)),
            size_bytes: 4096,
            change_type: "create".to_string(),
            created_at: 1_760_000_000,
            device_id: Some("ee".repeat(32)),
            author_actor_id: author.map(str::to_string),
            path_sealed: Some(fauna_protocol::ByteBuf::from(vec![1, 2, 3])),
            ..Default::default()
        }
    }

    /// A row `writer` signed directly under `nonce`.
    fn signed(writer: &ActorKeypair, nonce: [u8; 32]) -> SyncChange {
        let mut r = row(Some(&hex_of(writer)));
        ChangeSigner::direct(writer)
            .sign_row(&mut r, nonce)
            .unwrap();
        r
    }

    fn local(change: SyncChange, sequenced: bool, locally_authored: bool) -> LocalShareChange {
        LocalShareChange {
            change,
            sequenced,
            locally_authored,
            signer_cert: None,
        }
    }

    /// A reader for a set `owner` owns, whose roster lists `writers`.
    fn reader(owner: &ActorKeypair, writers: &[&ActorKeypair]) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(owner.actor_id().0),
            ..Default::default()
        });
        r.install_roster(
            writers
                .iter()
                .map(|k| k.actor_id().0)
                .collect::<std::collections::HashSet<_>>(),
        );
        r
    }

    /// A reader for a set `owner` owns that never read its roster — a member
    /// offline since launch.
    fn unread(owner: &ActorKeypair) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(owner.actor_id().0),
            ..Default::default()
        });
        r
    }

    /// The share leg's cached roster listing `writers`, with no chains.
    fn cached(writers: &[&ActorKeypair]) -> WriterRoster {
        writers
            .iter()
            .map(|k| k.actor_id().0)
            .collect::<std::collections::HashSet<_>>()
            .into()
    }

    // ── The serve filter ────────────────────────────────────────────────────

    #[test]
    fn own_sequenced_and_own_pending_rows_both_serve() {
        let us = kp(1);
        assert!(serves_held_row(
            &local(row(Some(&hex_of(&us))), true, true),
            &hex_of(&us)
        ));
        assert!(
            serves_held_row(&local(row(None), false, true), &hex_of(&us)),
            "a pre-signing own-pending row carries no stamp and still serves"
        );
        assert!(serves_held_row(
            &local(signed(&us, NONCE), false, true),
            &hex_of(&us)
        ));
    }

    /// The lift: a signed row of another writer is relayed — its signature is
    /// the proof that survives the hop.
    #[test]
    fn another_writers_signed_row_is_relayed() {
        let (us, them) = (kp(1), kp(2));
        assert!(serves_held_row(
            &local(signed(&them, NONCE), true, false),
            &hex_of(&us)
        ));
        assert!(
            serves_held_row(&local(signed(&them, NONCE), false, false), &hex_of(&us)),
            "another writer's pending row relays too — the offline N-member case"
        );
    }

    /// What stays refused: a relay nobody can vouch for.
    #[test]
    fn an_unsigned_row_of_someone_else_is_never_relayed() {
        let (us, them) = (kp(1), kp(2));
        assert!(!serves_held_row(
            &local(row(Some(&hex_of(&them))), true, false),
            &hex_of(&us)
        ));
        assert!(!serves_held_row(
            &local(row(None), false, false),
            &hex_of(&us)
        ));
    }

    /// A store answering `locally_authored` for a row a nest attributes to
    /// someone else is a store bug; the filter must not launder it.
    #[test]
    fn a_stamp_naming_someone_else_outranks_a_local_origin_claim() {
        let (us, them) = (kp(1), kp(2));
        assert!(!serves_held_row(
            &local(row(Some(&hex_of(&them))), true, true),
            &hex_of(&us)
        ));
    }

    #[test]
    fn a_sequenced_own_row_with_no_author_stamp_is_not_served() {
        assert!(
            !serves_held_row(&local(row(None), true, true), &hex_of(&kp(1))),
            "a pre-multi-writer row's recorder cannot be proven to be us"
        );
    }

    #[test]
    fn the_hex_comparison_is_case_insensitive() {
        let us = kp(1);
        assert!(serves_held_row(
            &local(row(Some(&hex_of(&us).to_uppercase())), true, true),
            &hex_of(&us)
        ));
    }

    // ── The binding-free screen ─────────────────────────────────────────────

    #[test]
    fn the_screen_passes_a_signed_row_on_and_holds_an_unsigned_one_to_the_channel_proof() {
        let (peer, other) = (kp(2), kp(3));
        assert_eq!(
            screen_peer_row(&signed(&other, NONCE), true, &hex_of(&peer), false),
            Ok(()),
            "only the reader can judge a signed row — the screen never refuses one"
        );
        assert_eq!(
            screen_peer_row(&row(Some(&hex_of(&other))), true, &hex_of(&peer), true),
            Err(RowRefusal::RelayedThirdPartyRow)
        );
        assert_eq!(
            screen_peer_row(&row(Some(&hex_of(&peer))), true, &hex_of(&peer), false),
            Err(RowRefusal::NotACachedWriter)
        );
        assert_eq!(
            screen_peer_row(&row(None), true, &hex_of(&peer), true),
            Err(RowRefusal::SequencedWithoutAuthor)
        );
    }

    // ── The authoritative judge ─────────────────────────────────────────────

    /// The lift: a third writer's signed row, served by a peer that is not its
    /// author, is admitted as that writer's.
    #[test]
    fn a_relayed_signed_row_of_a_writer_is_admitted() {
        let (owner, peer, writer) = (kp(1), kp(2), kp(3));
        let r = reader(&owner, &[&peer, &writer]);
        assert_eq!(
            judge_peer_row(&r, &signed(&writer, NONCE), true, &hex_of(&peer), false),
            Ok(PeerRowAdmission::Verified {
                writer: writer.actor_id().0
            })
        );
    }

    /// The read-only-member fabrication negative: a member with no writer role
    /// signs a row with its own key — the chain holds, the roster does not.
    #[test]
    fn a_non_writers_signed_row_is_refused_whoever_serves_it() {
        let (owner, peer, reader_only) = (kp(1), kp(2), kp(4));
        let r = reader(&owner, &[&peer]);
        assert_eq!(
            judge_peer_row(&r, &signed(&reader_only, NONCE), true, &hex_of(&peer), true),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::NotAWriter))
        );
    }

    /// A fabricated row: an altered signed field, or a row bound to another
    /// set's nonce — the signature does not cover the lie. And a served author
    /// that names someone else changes nothing either way (ruling (8)(a)): the
    /// row is the actor's whose signature it carries, never the stamp's.
    #[test]
    fn a_fabricated_or_altered_row_is_refused() {
        let (owner, peer, writer) = (kp(1), kp(2), kp(3));
        let r = reader(&owner, &[&peer, &writer]);
        let mut altered = signed(&writer, NONCE);
        altered.size_bytes += 1;
        assert!(matches!(
            judge_peer_row(&r, &altered, true, &hex_of(&peer), true),
            Err(RowRefusal::DidNotVerify(_))
        ));
        // A writer's row served under ANOTHER writer's name is admitted as
        // its true signer's — the stamp attributes nothing.
        let mut misattributed = signed(&peer, NONCE);
        misattributed.author_actor_id = Some(hex_of(&writer));
        assert_eq!(
            judge_peer_row(&r, &misattributed, true, &hex_of(&peer), true),
            Ok(PeerRowAdmission::Verified {
                writer: peer.actor_id().0
            })
        );
        // A NON-writer's row served under a writer's name is refused: the
        // stamp admits nobody.
        let stranger = kp(7);
        let mut laundered = signed(&stranger, NONCE);
        laundered.author_actor_id = Some(hex_of(&writer));
        assert_eq!(
            judge_peer_row(&r, &laundered, true, &hex_of(&peer), true),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::NotAWriter))
        );
        // …and before any roster read, the CACHED roster is asked about the
        // signed actor — never about the name the row was served under.
        let over_cache = reader_over_cached_roster(unread(&owner), cached(&[&writer]));
        assert_eq!(
            judge_peer_row(&over_cache, &laundered, true, &hex_of(&peer), true),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::NotAWriter))
        );
        assert!(
            matches!(
                judge_peer_row(&r, &signed(&writer, [9; 32]), true, &hex_of(&peer), true),
                Err(RowRefusal::DidNotVerify(_))
            ),
            "a row bound to another set's nonce is not this set's record"
        );
    }

    /// Every writer signs: an unsigned row is refused whoever serves it — even
    /// the serving peer's own, even a stampless own-pending one.
    #[test]
    fn an_unsigned_row_is_refused_even_as_the_serving_peers_own() {
        let (owner, peer, other) = (kp(1), kp(2), kp(3));
        let r = reader(&owner, &[&peer, &other]);
        let peer_hex = hex_of(&peer);
        for (change, sequenced) in [
            (row(Some(&peer_hex)), true),
            (row(None), false),
            (row(Some(&hex_of(&other))), true),
        ] {
            assert_eq!(
                judge_peer_row(&r, &change, sequenced, &peer_hex, true),
                Err(RowRefusal::DidNotVerify(ChangeVerifyError::Unsigned))
            );
        }
    }

    /// A class the signature check does not apply to has the channel's proof
    /// alone: the serving peer's own row, from a cached writer, or nothing.
    #[test]
    fn an_exempt_class_row_is_admitted_only_as_the_serving_peers_own() {
        let (owner, peer, other) = (kp(1), kp(2), kp(3));
        let r = reader(&owner, &[&peer, &other]);
        let peer_hex = hex_of(&peer);
        let retention = |author: Option<&str>| SyncChange {
            is_retention: Some(true),
            ..row(author)
        };
        assert_eq!(
            judge_peer_row(&r, &retention(Some(&peer_hex)), true, &peer_hex, true),
            Ok(PeerRowAdmission::ChannelProven)
        );
        assert_eq!(
            judge_peer_row(&r, &retention(Some(&hex_of(&other))), true, &peer_hex, true),
            Err(RowRefusal::RelayedThirdPartyRow),
            "a relayed exempt row is refused"
        );
        assert_eq!(
            judge_peer_row(&r, &retention(Some(&peer_hex)), true, &peer_hex, false),
            Err(RowRefusal::NotACachedWriter),
            "the fail-closed arm: no cached role for the serving peer"
        );
    }

    /// An item class routes a row away from the file reader, not past its
    /// check (`writer-signed-change-records.md` ruling (3)): a `state-entry`
    /// or `record-cid` row is no file row over the peer door either — not
    /// even as the serving peer's own, signed or not.
    #[test]
    fn an_item_class_row_is_refused_even_as_the_serving_peers_own() {
        let (owner, peer) = (kp(1), kp(2));
        let r = reader(&owner, &[&peer]);
        let peer_hex = hex_of(&peer);
        for class in [
            fauna_protocol::account_state::ItemClass::StateEntry,
            fauna_protocol::account_state::ItemClass::RecordCid,
        ] {
            for mut change in [row(Some(&peer_hex)), signed(&peer, NONCE)] {
                change.item_class = Some(class.as_wire().into());
                assert_eq!(
                    judge_peer_row(&r, &change, true, &peer_hex, true),
                    Err(RowRefusal::DidNotVerify(ChangeVerifyError::OtherPlane)),
                    "{class:?}"
                );
            }
        }
    }

    /// A reader that never read its roster places a verified signer by the
    /// share leg's cached roster, for the SIGNED actor — never the server's.
    #[test]
    fn with_no_roster_read_the_cached_roster_places_the_signed_actor() {
        let (owner, peer, writer) = (kp(1), kp(2), kp(3));
        let over = |cache: WriterRoster| reader_over_cached_roster(unread(&owner), cache);
        assert_eq!(
            judge_peer_row(
                &over(cached(&[&writer])),
                &signed(&writer, NONCE),
                true,
                &hex_of(&peer),
                false
            ),
            Ok(PeerRowAdmission::Verified {
                writer: writer.actor_id().0
            })
        );
        assert_eq!(
            judge_peer_row(
                &over(cached(&[&peer])),
                &signed(&writer, NONCE),
                true,
                &hex_of(&peer),
                true
            ),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::NotAWriter)),
            "the serving peer's role vouches for nobody else's row"
        );
        // No roster stands at all — never read, nothing cached: fail closed,
        // whatever the serving peer's role is said to be.
        for reader in [unread(&owner), over(WriterRoster::default())] {
            assert_eq!(
                judge_peer_row(&reader, &signed(&writer, NONCE), true, &hex_of(&peer), true),
                Err(RowRefusal::NotACachedWriter)
            );
        }
        // A roster the reader read itself is never replaced by the cache.
        let read = reader_over_cached_roster(reader(&owner, &[&peer]), cached(&[&writer]));
        assert_eq!(
            judge_peer_row(&read, &signed(&writer, NONCE), true, &hex_of(&peer), true),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::NotAWriter))
        );
    }

    /// The cut on the peer leg (`writer-signed-change-records.md` ruling
    /// (11)(c)/(i)): over the cached roster, which stores the owner's
    /// predecessor beside the owner, a member's unread reader gives a
    /// peer-served row exactly the nest pull's verdict.
    #[test]
    fn over_the_cached_chain_a_predecessors_row_takes_the_nest_pulls_verdict() {
        const RETIRED: [u8; 32] = [5; 32];
        let (owner, old_owner, peer, member) = (kp(1), kp(8), kp(2), kp(3));
        let mut member_reader = RowReader::new();
        member_reader.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(owner.actor_id().0),
            live_minted_by: Some(owner.actor_id().0),
            retired_set_nonces: vec![(RETIRED, Some(old_owner.actor_id().0))],
            ..Default::default()
        });
        let mut cache = cached(&[&owner, &peer, &member]);
        cache
            .predecessors
            .insert(owner.actor_id().0, vec![old_owner.actor_id().0]);
        let peer_hex = hex_of(&peer);
        let judge = |cache: &WriterRoster, row: &SyncChange| {
            let reader = reader_over_cached_roster(member_reader.clone(), cache.clone());
            judge_peer_row(&reader, row, true, &peer_hex, true)
        };

        assert_eq!(
            judge(&cache, &signed(&old_owner, NONCE)),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::HistoryEra)),
            "arm (1): the live nonce was minted after the predecessor retired — a plant"
        );
        assert_eq!(
            judge(&cache, &signed(&old_owner, RETIRED)),
            Err(RowRefusal::History),
            "arm (2): the predecessor's row under a retired nonce is history"
        );
        assert_eq!(
            judge(&cache, &signed(&member, RETIRED)),
            Ok(PeerRowAdmission::Verified {
                writer: member.actor_id().0
            }),
            "arm (3): a cut touches nothing a member signed"
        );
        assert_eq!(
            judge(&cache, &signed(&owner, RETIRED)),
            Ok(PeerRowAdmission::Verified {
                writer: owner.actor_id().0
            }),
            "arm (3): the current owner's own rows are current under every nonce"
        );

        // Membership in the chain is decided before ruling (8)(b)'s
        // precedence: a roster that also lists the predecessor as a writer in
        // its own right lifts it out of nothing.
        let mut listed = cache.clone();
        listed.writers.insert(old_owner.actor_id().0);
        assert_eq!(
            judge(&listed, &signed(&old_owner, NONCE)),
            Err(RowRefusal::DidNotVerify(ChangeVerifyError::HistoryEra))
        );
        assert_eq!(
            judge(&listed, &signed(&old_owner, RETIRED)),
            Err(RowRefusal::History)
        );
    }

    /// The two halves must agree: whatever a conforming serve side sends, the
    /// screen passes — otherwise the leg would deadlock on legitimate traffic.
    #[test]
    fn a_conforming_serve_sides_output_always_passes_the_screen() {
        let (us, them) = (kp(1), kp(2));
        let candidates = [
            row(None),
            row(Some(&hex_of(&us))),
            row(Some(&hex_of(&them))),
            signed(&us, NONCE),
            signed(&them, NONCE),
        ];
        for change in candidates {
            for sequenced in [true, false] {
                for locally_authored in [true, false] {
                    let candidate = local(change.clone(), sequenced, locally_authored);
                    if serves_held_row(&candidate, &hex_of(&us)) {
                        assert_eq!(
                            screen_peer_row(&candidate.change, sequenced, &hex_of(&us), true),
                            Ok(()),
                            "served but refused on ingest: {candidate:?}"
                        );
                    }
                }
            }
        }
    }
}

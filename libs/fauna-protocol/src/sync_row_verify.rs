//! **The reader half of writer-signed change records** —
//! `docs/goal/architecture/mls-group-key-material.md` § M2 → *Multi-writer* →
//! *Writer-signed change records*, ruling (3): every row a client reader
//! consumes is judged here before fold, apply, materialize or re-seal. "A
//! record that does not verify is not a record."
//!
//! **The one judge every client reader shares** — the sync engine's nest pull,
//! the p2p share leg's ingest (`fauna_peer_share::provenance`), the Media and
//! version-history readers — which is why it lives here, beside the statement,
//! and not in any one reader's crate: pure, no I/O, wasm-clean.
//!
//! One judge ([`RowReader::judge`]) over one row, with everything it needs held
//! beside it: the set's binding (its nonce, its owner, its serve flag), the
//! delegation certs every list reply carries (`signer_certs`), and the writer
//! roster the engine's ungated roster leg reads. The shared statement and chain
//! are [`crate::sync_writer_sig`]'s; this module adds only what is the
//! reader's to decide — class routing, *this set*, *may they*, and what a row
//! that cannot be verified yet costs.
//!
//! **A row signed under a retired identity** (ruling (8)): the nest's stamp is
//! a first candidate only — the signed actor **A** is recovered from the
//! signature ([`sig::recover_signed_actor`]) and admitted iff it resolves to a
//! current writer **W**, as W itself or as a *proven* predecessor of W. The
//! verdict names both ([`RowVerdict::Verified`]): W for attribution, A for
//! everything that asks what this signature itself vouches for (certs, own
//! echo, which owner root may open the row's bytes or label).
//!
//! The non-admitting verdicts split two ways, because the two failures cost
//! different things. A **refused** row is treated as absent: the cursor advances past it,
//! the path frontier does not. A **held** row is one this reader cannot judge
//! *yet* — its roster was never read — so the pull stops below it and retries
//! (nothing is lost; the ruling's "refused until the first read succeeds"). An
//! unsigned row outside the class exemptions is refused, exactly as the nest
//! refuses to record one (`signature_required`): every writer signs.
//!
//! **Two refusals differ** (ruling (8)(f)). A row that fails cryptographically
//! is *not a record*. A row that verifies under this set's nonce but whose
//! signed actor resolves to no writer on this reader, now, is *a record this
//! reader cannot attribute yet* ([`RowVerdict::unattributed`]): a newly
//! granted writer met with a stale roster, a predecessor whose link this
//! reader has not proven. Both are refused; only the second is worth a roster
//! re-read before the refusal stands, and a later judgement once what the
//! reader admits by ([`RowReader::admitted_signers`]) gains something.

use std::collections::{HashMap, HashSet};

use crate::sync::SyncChange;
use crate::sync_writer_sig::{self as sig, ChangeVerifyError, SET_NONCE_LEN, SignerCertCache};
use fauna_core::encoding::{AuthoringOrigin, EmbedAsBytes};

/// What a reader knows about the set it reads — installed at engine build from
/// the resolved binding, re-installed whenever a re-push carries custody.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReaderBinding {
    /// The set's live nonce (custody for the owner, the content-key envelope
    /// for a member). `None` = custody holds none yet: no row can be verified.
    pub set_nonce: Option<[u8; SET_NONCE_LEN]>,
    /// The set's owner — this account for an owned set, the channel's
    /// MLS-recorded owner for a member. `None` on a member's host that holds
    /// no marker: the judge then reads the owner off the roster's owner row
    /// ([`WriterRoster::owner`], ruling (11)(c)) — never off the list row's
    /// `owner_actor_id`. The owner is always a writer, roster or not.
    pub owner: Option<[u8; 32]>,
    /// The set is WebDAV-served: its pseudo-device rows are the nest's word by
    /// construction (ruling (1)'s class exemption, residual (vi)).
    pub webdav_served: bool,
    /// The READING account's current identity — the writer a row signed as one
    /// of [`account_predecessors`](Self::account_predecessors) resolves to
    /// (when this account may write the set at all). `None` = no own-account
    /// source: a retired identity then resolves through the roster's carried
    /// statements alone.
    pub account: Option<[u8; 32]>,
    /// The reading account's **proven** predecessor ids (ruling (8)(b), source
    /// (ii)): the registry's possession-attested set
    /// (`AccountRegistry::attested_predecessor_actor_ids`), or what the same
    /// statement walk the roster runs proved for [`account`](Self::account).
    /// Never a nest-asserted list, never a self-asserted `prior_actor_ids` list.
    pub account_predecessors: Vec<[u8; 32]>,
    /// The identity whose device minted [`set_nonce`](Self::set_nonce)
    /// (`writer-signed-change-records.md` ruling (11)(a)) — custody's
    /// `minted_by` on the owner's hosts, the envelope's on a member's. `None`
    /// where none was recorded: it reads as the earliest identity in the
    /// owner's chain, so an un-cut set admits its predecessors' rows exactly
    /// as ruling (8) has it.
    pub live_minted_by: Option<[u8; 32]>,
    /// The set's **lineage** of retired nonces (ruling (11)(b)), each with its
    /// minter (`None` where none was recorded): a row that does not verify
    /// under the live nonce is tried under each, and one signed as a strict
    /// predecessor of the owner is history there ([`RowVerdict::History`]).
    pub retired_set_nonces: Vec<([u8; SET_NONCE_LEN], Option<[u8; 32]>)>,
    /// The owner's proven predecessors **in order, nearest first**, built from
    /// statement links (ruling (11)(c): which of two identities is the earlier
    /// is a question only an ordered chain answers). Filled on the owner's
    /// hosts from the account's recorded successions; empty on a member's,
    /// whose chain is the roster's owner row
    /// ([`WriterRoster::predecessors`]). Empty everywhere → the chain is read
    /// off [`account_predecessors`](Self::account_predecessors) when this
    /// account owns the set, else off the roster.
    pub owner_chain: Vec<[u8; 32]>,
}

/// Why a row cannot be judged yet — the pull holds below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// A signed row by a non-owner, and this reader never read the roster —
    /// same-nest or cross-nest alike (the cross-nest read relays to the set's
    /// home nest): fail closed without losing the row. Also a served set's
    /// unsigned row on a reader that holds no owner yet and never read the
    /// roster that names one.
    RosterUnread,
    /// The reader holds no nonce: nothing can verify until custody delivers
    /// one.
    NoNonce,
    /// A row signed as a strict predecessor of the owner, under a live nonce
    /// whose minter this reader cannot place in the owner's chain
    /// (`writer-signed-change-records.md` ruling (11)(c), arm (1)): whether it
    /// is an inheritance or a plant turns on an order this reader does not
    /// hold yet — the roster is re-read, as for a writer it cannot attribute.
    MinterUnplaced,
}

/// One row's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowVerdict {
    /// Signed, chained to `signed_as` with `SyncWrite`, bound to this set, and
    /// `signed_as` resolves to the writer `writer` (ruling (8)(c)).
    Verified {
        /// **W** — the current identity the row is attributed to: the signed
        /// actor itself, or the writer it is a proven predecessor of. For
        /// attribution only (version history, a judged projection's author).
        writer: [u8; 32],
        /// **A** — the identity the row was signed as. Everything that asks
        /// what this signature itself vouches for reads this: the cert is
        /// cached and relayed under it ([`RowReader::cert_for`]), a row is a
        /// device's own echo only when it is the reader's current identity,
        /// and only then is the current owner root offered to the row's bytes
        /// or label.
        signed_as: [u8; 32],
        origin: AuthoringOrigin,
    },
    /// A class the signature check does not apply to that a file reader still
    /// consumes or accounts (the fold's retention row, a served set's WebDAV
    /// pseudo-device row). An item-class row is no such class here — it is
    /// refused ([`ChangeVerifyError::OtherPlane`], ruling (3)).
    Exempt,
    /// Not a record: skipped, warned, counted; the cursor passes it.
    Refused(ChangeVerifyError),
    /// Not judgeable yet: the pull stops below it.
    Held(Held),
    /// **History** (`writer-signed-change-records.md` ruling (11)(c), arm
    /// (2)): verified under a RETIRED nonce of the set's lineage, signed as a
    /// strict predecessor of the set's owner — a version of the path, listed
    /// under `signed_as` and nothing else: never a head, never folded, never
    /// re-recorded but by the take-over. `writer` and `signed_as` are what a
    /// [`Self::Verified`] row would carry, so attribution and the root bound
    /// of ruling (8)(c) are unchanged for a restore that later opens it.
    History {
        writer: [u8; 32],
        signed_as: [u8; 32],
        origin: AuthoringOrigin,
        /// The retired nonce the row verified under.
        nonce: [u8; SET_NONCE_LEN],
    },
}

impl RowVerdict {
    /// Whether the row may be consumed (folded, applied, materialized). Never
    /// for [`Self::History`].
    pub fn admits(&self) -> bool {
        matches!(self, Self::Verified { .. } | Self::Exempt)
    }

    /// Whether the row is history (ruling (11)(c)) — what the version-history
    /// and Media projections read to list it as a version, under `signed_as`,
    /// and never as the item.
    pub fn is_history(&self) -> bool {
        matches!(self, Self::History { .. })
    }

    /// The second of the two refusals (ruling (8)(f)): **a record this reader
    /// cannot attribute yet** — the row verified under this set's nonce, and
    /// the actor it was signed as resolves to no writer on this reader, now.
    /// Every other refusal is *not a record*. [`RowReader::judge`] answers
    /// [`ChangeVerifyError::NotAWriter`] only once the signature has
    /// recovered its signer, which is what makes that error the class.
    pub fn unattributed(&self) -> bool {
        matches!(self, Self::Refused(ChangeVerifyError::NotAWriter))
    }

    /// Whether a roster read could change this verdict — a row held for want
    /// of one ([`Held::RosterUnread`]), a nonce minter it has not placed yet
    /// ([`Held::MinterUnplaced`], ruling (11)(c)), or a record this reader
    /// cannot attribute yet ([`Self::unattributed`]). What every reader asks
    /// before it re-reads the roster and judges again.
    pub fn wants_roster_read(&self) -> bool {
        matches!(
            self,
            Self::Held(Held::RosterUnread) | Self::Held(Held::MinterUnplaced)
        ) || self.unattributed()
    }

    /// Whether the row verified as signed under `identity` itself — the one
    /// fact that licenses the **current** owner root for the row's bytes and
    /// label (ruling (8)(c)). `false` for a row signed as a predecessor
    /// (whoever it is attributed to), another writer's row, an exempt row and
    /// every non-admitting verdict; `false` for a reader with no identity.
    pub fn signed_as(&self, identity: Option<&[u8; 32]>) -> bool {
        matches!(
            (self, identity),
            (Self::Verified { signed_as, .. }, Some(identity)) if signed_as == identity
        )
    }
}

/// One roster read's writers, with what each writer's carried statements
/// prove about its retired identities — what [`RowReader::install_roster`]
/// takes ([`writer_roster`] builds it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriterRoster {
    /// The `writer`-access members (the owner included when the nest lists
    /// one).
    pub writers: HashSet<[u8; 32]>,
    /// For a writer whose row carried a chain that **verified**: the
    /// predecessor ids it proves, nearest hop first. A writer with no chain,
    /// or one that proved nothing, has no entry.
    pub predecessors: HashMap<[u8; 32], Vec<[u8; 32]>>,
    /// The actor of the read's one `role == "owner"` row — the set's owner
    /// for a reader whose binding names none (ruling (11)(c): a host that
    /// holds no marker), and for no other reader: an owner the binding names
    /// is never moved by this. `None` when the read carried no owner row, an
    /// undecodable one, or more than one (fail closed).
    pub owner: Option<[u8; 32]>,
}

impl WriterRoster {
    /// The writers alone, every proven predecessor dropped — for a reader that
    /// does not yet act on a retired identity's rows.
    pub fn without_predecessors(self) -> Self {
        Self {
            writers: self.writers,
            predecessors: HashMap::new(),
            owner: self.owner,
        }
    }
}

impl From<HashSet<[u8; 32]>> for WriterRoster {
    fn from(writers: HashSet<[u8; 32]>) -> Self {
        Self {
            writers,
            predecessors: HashMap::new(),
            owner: None,
        }
    }
}

/// A reader's verification state for one set.
#[derive(Debug, Clone)]
pub struct RowReader {
    binding: ReaderBinding,
    certs: SignerCertCache,
    /// The last successful roster read. `None` = never read.
    roster: Option<WriterRoster>,
}

impl Default for RowReader {
    fn default() -> Self {
        Self::new()
    }
}

impl RowReader {
    pub fn new() -> Self {
        Self {
            binding: ReaderBinding::default(),
            certs: SignerCertCache::new(),
            roster: None,
        }
    }

    pub fn binding(&self) -> &ReaderBinding {
        &self.binding
    }

    pub fn install_binding(&mut self, binding: ReaderBinding) {
        self.binding = binding;
    }

    /// Feed a reply's `signer_certs` side table (a different cert for a
    /// cached key replaces it — [`SignerCertCache::ingest`]).
    pub fn ingest_certs<'a>(&mut self, certs: impl IntoIterator<Item = &'a EmbedAsBytes>) {
        self.certs.ingest_all(certs);
    }

    /// The carried cert a verified row's `signer_key` chained through — what a
    /// holder keeps beside a row it retains for relay, so the row stays
    /// self-contained. `None` for a direct signer. `signed_as` is the
    /// verdict's **signed** actor (ruling (8)(c)): the cert names, and is
    /// cached under, the identity that root-signed it — never the writer a
    /// retired identity is attributed to.
    pub fn cert_for(&self, signed_as: &[u8; 32], signer_key: &[u8; 32]) -> Option<&EmbedAsBytes> {
        self.certs.get(signed_as, signer_key)
    }

    /// Replace the roster with a successful read's.
    pub fn install_roster(&mut self, roster: impl Into<WriterRoster>) {
        self.roster = Some(roster.into());
    }

    /// Whether a roster read ever succeeded.
    pub fn roster_read(&self) -> bool {
        self.roster.is_some()
    }

    /// **Who the set's owner is, to this judge** (`writer-signed-change-records.md`
    /// ruling (11)(c)): the binding's owner where it names one — the roster's
    /// owner row never moves that — else the owner row of the last roster
    /// read, on a host that holds no marker. `None`: neither names one yet.
    /// Every owner question the judge asks goes through here; whether this
    /// ACCOUNT owns the set is the binding's alone to say.
    fn owner(&self) -> Option<[u8; 32]> {
        self.binding
            .owner
            .or_else(|| self.roster.as_ref().and_then(|r| r.owner))
    }

    /// Whether `actor` is itself a writer of this set as far as this reader
    /// knows — the owner, or a `writer` on the last roster read.
    pub fn is_writer(&self, actor: &[u8; 32]) -> bool {
        self.owner().as_ref() == Some(actor)
            || self
                .roster
                .as_ref()
                .is_some_and(|r| r.writers.contains(actor))
    }

    /// The predecessor ids proven for the writer `writer`, by either source of
    /// ruling (8)(b): the reading account's own (when `writer` is that
    /// account), and the roster row's verified chain.
    fn proven_predecessors_of(&self, writer: &[u8; 32]) -> impl Iterator<Item = &[u8; 32]> {
        let chained = (self.owner().as_ref() == Some(writer))
            .then_some(&self.binding.owner_chain)
            .into_iter()
            .flatten();
        let own = (self.binding.account.as_ref() == Some(writer))
            .then_some(&self.binding.account_predecessors)
            .into_iter()
            .flatten();
        let rostered = self
            .roster
            .as_ref()
            .and_then(|r| r.predecessors.get(writer))
            .into_iter()
            .flatten();
        chained.chain(own).chain(rostered)
    }

    /// **The owner's chain** below the owner, nearest first
    /// (`writer-signed-change-records.md` ruling (11)(c)): the binding's
    /// ordered [`owner_chain`](ReaderBinding::owner_chain) when one was
    /// installed, else this account's own proven predecessors when it owns
    /// the set, then whatever the roster's owner row proves beyond those.
    /// Membership is decided by statements alone, before ruling (8)(b)'s
    /// precedence: a predecessor the roster also lists as a writer stays in
    /// the chain.
    fn owner_predecessors(&self) -> Vec<[u8; 32]> {
        let Some(owner) = self.owner() else {
            return Vec::new();
        };
        let primary: &[[u8; 32]] = if !self.binding.owner_chain.is_empty() {
            &self.binding.owner_chain
        } else if self.binding.account == Some(owner) {
            &self.binding.account_predecessors
        } else {
            &[]
        };
        let rostered = self
            .roster
            .as_ref()
            .and_then(|r| r.predecessors.get(&owner))
            .into_iter()
            .flatten();
        let mut chain: Vec<[u8; 32]> = Vec::new();
        for id in primary.iter().chain(rostered) {
            if *id != owner && !chain.contains(id) {
                chain.push(*id);
            }
        }
        chain
    }

    /// **What this reader admits by** — every identity a row may be signed as
    /// and resolve to a writer ([`Self::resolve_writer`]): the owner, each
    /// `writer` on the last roster read, and every proven predecessor of one
    /// of those. What a reader with a cursor compares across time (ruling
    /// (8)(f)): an id here that was not here before is a *gain*, and rows the
    /// cursor passed while it was missing are worth judging again.
    pub fn admitted_signers(&self) -> std::collections::BTreeSet<[u8; 32]> {
        let owner = self.owner();
        let writers = owner
            .iter()
            .chain(self.roster.iter().flat_map(|r| r.writers.iter()));
        let mut admitted = std::collections::BTreeSet::new();
        for writer in writers {
            admitted.insert(*writer);
            admitted.extend(self.proven_predecessors_of(writer).copied());
        }
        admitted
    }

    /// The current writer the signed actor resolves to — ruling (8)(b). The
    /// signed actor as a writer itself wins over any chain that claims it;
    /// else the one writer it is a proven predecessor of; and where chains
    /// ending at two different writers both claim it, the row is attributed to
    /// the signed actor, to neither of them — so no served link can
    /// re-attribute a row. `None`: it resolves to no writer.
    fn resolve_writer(&self, signed_as: &[u8; 32]) -> Option<[u8; 32]> {
        if self.is_writer(signed_as) {
            return Some(*signed_as);
        }
        let owner = self.owner();
        let candidates = self
            .binding
            .account
            .iter()
            .chain(owner.iter())
            .chain(self.roster.iter().flat_map(|r| r.predecessors.keys()));
        let mut claimants: Vec<[u8; 32]> = Vec::new();
        for writer in candidates {
            if !claimants.contains(writer)
                && self.is_writer(writer)
                && self.proven_predecessors_of(writer).any(|p| p == signed_as)
            {
                claimants.push(*writer);
            }
        }
        match claimants.as_slice() {
            [] => None,
            [writer] => Some(*writer),
            _ => Some(*signed_as),
        }
    }

    /// The actor `row` was signed as, where its signature verifies under this
    /// reader's nonce — whoever that is, writer or not. `None` for an unsigned
    /// row, a row that does not verify, and a reader holding no nonce. What a
    /// reader asks before it goes looking for the link that would place a
    /// row it refused `NotAWriter`.
    pub fn signed_actor(&self, row: &SyncChange) -> Option<[u8; 32]> {
        sig::recover_signed_actor(row, self.binding.set_nonce?, &self.certs)
            .ok()
            .map(|signed| signed.signed_as)
    }

    /// Ruling (1)(i)'s class exemption as (8)(d) reads it: the pseudo-device of
    /// the set's owner **or of a proven predecessor of the owner**, on a served
    /// set. The pseudo-device id is a pure function of an actor id, so the
    /// rows recorded before a succession carry the predecessor's.
    fn is_webdav_pseudo_row(&self, row: &SyncChange) -> bool {
        let (true, Some(owner), Some(device)) = (
            self.binding.webdav_served,
            self.owner(),
            row.device_id.as_deref(),
        ) else {
            return false;
        };
        let Ok(device) = fauna_core::hex32::decode(device) else {
            return false;
        };
        std::iter::once(&owner)
            .chain(self.proven_predecessors_of(&owner))
            .any(|actor| device == fauna_core::label_custody::webdav_pseudo_device_id(actor))
    }

    /// Judge one served row, as served — BEFORE any receiver rewrite of a
    /// signed field (the watermark bound rewrites `derived_through`).
    pub fn judge(&self, row: &SyncChange) -> RowVerdict {
        self.judge_classed(row, true)
    }

    /// [`Self::judge`], with the retention exemption taken only when
    /// `retention_exempt` — the fold's reading; a reader of versions passes
    /// `false` (ruling (10)(e)).
    fn judge_classed(&self, row: &SyncChange, retention_exempt: bool) -> RowVerdict {
        // Class routing precedes the signature check — and an item class
        // routes a row away from the file reader, not past its check (ruling
        // (3)): every reader of this judge reads the file plane, so a
        // `state-entry` or `record-cid` row is no row of its, signed or not.
        let exempt = match sig::exempt_class(row) {
            Some(sig::ExemptClass::Retention) => retention_exempt,
            Some(sig::ExemptClass::StateEntry | sig::ExemptClass::RecordCid) => {
                return RowVerdict::Refused(ChangeVerifyError::OtherPlane);
            }
            None => self.is_webdav_pseudo_row(row),
        };
        if exempt {
            return RowVerdict::Exempt;
        }
        if row.signature.is_none() || row.signer_key.is_none() {
            // A served set's pseudo-device row is exempt only as the owner's,
            // and a reader that holds no owner and never read its roster does
            // not know the owner yet: the row holds for that one read, so the
            // cursor passes nothing the read would exempt.
            if self.binding.webdav_served
                && row.device_id.is_some()
                && self.roster.is_none()
                && self.owner().is_none()
            {
                return RowVerdict::Held(Held::RosterUnread);
            }
            return RowVerdict::Refused(ChangeVerifyError::Unsigned);
        }
        let Some(set_nonce) = self.binding.set_nonce else {
            return RowVerdict::Held(Held::NoNonce);
        };
        // Who signed, and as whom — from the signature, never the stamp. The
        // live nonce first; only a row that fails there is tried under each
        // retired nonce of the lineage (ruling (11)(b)). Whatever the live
        // failure: a row the nest moved after a succession is served under
        // the successor as author, so under the live nonce that candidate
        // fails its chain (`CertMissing`) before the signer's own is reported,
        // and a predecessor's row under a retired nonce must still reach the
        // lineage. A row no retired nonce recovers keeps the live refusal.
        match sig::recover_signed_actor(row, set_nonce, &self.certs) {
            Ok(signed) => self.judge_live(row, signed),
            Err(e) => self
                .binding
                .retired_set_nonces
                .iter()
                .find_map(|(nonce, _)| {
                    sig::recover_signed_actor(row, *nonce, &self.certs)
                        .ok()
                        .map(|signed| self.judge_retired(row, signed, *nonce))
                })
                .unwrap_or(RowVerdict::Refused(e)),
        }
    }

    /// Where `actor` sits in the owner's chain: `Some(0)` for the owner, `Some(i
    /// + 1)` for the `i`th predecessor nearest first — a larger position is an
    /// earlier identity. `None`: outside the chain.
    fn chain_position(
        owner: &[u8; 32],
        predecessors: &[[u8; 32]],
        actor: &[u8; 32],
    ) -> Option<usize> {
        if actor == owner {
            return Some(0);
        }
        predecessors.iter().position(|p| p == actor).map(|i| i + 1)
    }

    /// Ruling (11)(c) under the LIVE nonce. Arm (1): a row signed as a strict
    /// predecessor A of the owner is refused as history-era when the nonce's
    /// minter is a strict successor of A — the nonce was minted after A
    /// retired, so A never legitimately signed under it — current when the
    /// minter is A itself or earlier (or unrecorded: the earliest), and held
    /// when the minter cannot be placed. Arm (3): anyone else, as ruling (8)
    /// has it.
    fn judge_live(&self, row: &SyncChange, signed: sig::SignedRow) -> RowVerdict {
        if let Some(owner) = self.owner() {
            let predecessors = self.owner_predecessors();
            if let Some(a) = predecessors.iter().position(|p| *p == signed.signed_as) {
                let a = a + 1;
                if let Some(minter) = self.binding.live_minted_by {
                    match Self::chain_position(&owner, &predecessors, &minter) {
                        Some(m) if m < a => {
                            return RowVerdict::Refused(ChangeVerifyError::HistoryEra);
                        }
                        Some(_) => {}
                        None => return RowVerdict::Held(Held::MinterUnplaced),
                    }
                }
            }
        }
        self.admit(row, signed)
    }

    /// Ruling (11)(c) under a RETIRED nonce of the lineage. Arm (2): a row
    /// signed as a strict predecessor of the owner is history. Arm (3): the
    /// current owner's own rows and every row signed outside the chain are
    /// current under a retired nonce as under the live one — no thief signs as
    /// the current identity, and a cut touches nothing a member signed.
    fn judge_retired(
        &self,
        row: &SyncChange,
        signed: sig::SignedRow,
        nonce: [u8; SET_NONCE_LEN],
    ) -> RowVerdict {
        let history = self.owner().is_some_and(|owner| {
            signed.signed_as != owner && self.owner_predecessors().contains(&signed.signed_as)
        });
        if !history {
            return self.admit(row, signed);
        }
        if let Some(refused) = self.plaintext_path_refusal(row) {
            return refused;
        }
        RowVerdict::History {
            writer: self
                .resolve_writer(&signed.signed_as)
                .or(self.owner())
                .unwrap_or(signed.signed_as),
            signed_as: signed.signed_as,
            origin: signed.origin,
            nonce,
        }
    }

    /// A reader that places by the plaintext `path` checks it against the
    /// signed hash: the plaintext path is not in the statement. The honest
    /// nest serves one on the public plane only (no `path_sealed`), but a
    /// reader places by any non-empty one it is served — ahead of opening the
    /// sealed label — so every such path is checked, sealed label or not.
    fn plaintext_path_refusal(&self, row: &SyncChange) -> Option<RowVerdict> {
        let served = row.path.as_deref().is_some_and(|p| !p.is_empty());
        (served && !sig::plaintext_path_matches(row)).then(|| {
            RowVerdict::Refused(ChangeVerifyError::Malformed(
                "plaintext path does not match the signed path_hash".into(),
            ))
        })
    }

    /// May they: the signed actor as a writer, or as a proven predecessor of
    /// one (ruling (8)) — the verdict of a row whose nonce placed it current.
    fn admit(&self, row: &SyncChange, signed: sig::SignedRow) -> RowVerdict {
        let Some(writer) = self.resolve_writer(&signed.signed_as) else {
            return match self.roster {
                None => RowVerdict::Held(Held::RosterUnread),
                Some(_) => RowVerdict::Refused(ChangeVerifyError::NotAWriter),
            };
        };
        if let Some(refused) = self.plaintext_path_refusal(row) {
            return refused;
        }
        RowVerdict::Verified {
            writer,
            signed_as: signed.signed_as,
            origin: signed.origin,
        }
    }

    /// Judge a row a reader holds only as a PROJECTION — a Media item or a
    /// version-history entry, rebuilt through its `as_change_row`. `None` is a
    /// projection that carries no statement (a reader outside the set's label
    /// audience, to whom the nest projects none of the signed fields): nothing
    /// can verify it, so it is judged exactly as an unsigned row — refused.
    ///
    /// A projection takes no retention exemption (ruling (10)(e)): a conflict
    /// report's retained loser is a version only when its reporter signed it
    /// (the flag is in the statement), so an unsigned retention row is refused
    /// here while the fold ([`Self::judge`]) still accounts and skips it.
    pub fn judge_projection(&self, row: Option<&SyncChange>) -> RowVerdict {
        match row {
            Some(row) => self.judge_classed(row, false),
            None => RowVerdict::Refused(ChangeVerifyError::Unsigned),
        }
    }
}

/// The writer predicate over one actor-roster member — `role == "owner"` (the
/// owner holds no grant; they own) or `access == "writer"`. One definition for
/// the reader's roster leg and the share leg's cached roster.
pub fn roster_member_is_writer(role: &str, access: Option<&str>) -> bool {
    role == "owner" || access == Some("writer")
}

/// The writers one actor-roster read (`fauna.folders.members.list_actors`)
/// names, each with the predecessor ids its carried statements **prove** —
/// what [`RowReader::install_roster`] takes. An undecodable actor id is no
/// writer. The one reading of that reply for every roster leg (the engine's,
/// the projection readers').
///
/// Ruling (8)(b): the link from a retired identity to a writer is proven,
/// never projected. A writer's
/// [`succession_statements`](crate::folders::FolderActorMember::succession_statements)
/// count only as an unbroken chain ending at that writer, every link's
/// `new_sig` verifying under that link's own successor
/// (`fauna_core::recovery::proven_predecessors_carried`); a chain that is
/// broken, ends elsewhere, or carries a link another key signed proves
/// nothing, and the writer simply has no predecessors here. The nest still
/// decides who is a writer; it cannot decide whose predecessor anyone is.
pub fn writer_roster(members: &[crate::folders::FolderActorMember]) -> WriterRoster {
    let mut roster = WriterRoster::default();
    // The owner a marker-less reader takes (ruling (11)(c)): exactly one
    // owner row, or none.
    let mut owner_rows = members.iter().filter(|m| m.role == "owner");
    if let (Some(owner), None) = (owner_rows.next(), owner_rows.next()) {
        roster.owner = fauna_core::hex32::decode(&owner.actor_id).ok();
    }
    for member in members
        .iter()
        .filter(|m| roster_member_is_writer(&m.role, m.access.as_deref()))
    {
        let Ok(writer) = fauna_core::hex32::decode(&member.actor_id) else {
            continue;
        };
        roster.writers.insert(writer);
        let proven = fauna_core::recovery::proven_predecessors_carried(
            &fauna_core::identity::ActorId(writer),
            &member.succession_statements,
        );
        if !proven.is_empty() {
            roster
                .predecessors
                .insert(writer, proven.into_iter().map(|a| a.0).collect());
        }
    }
    roster
}

/// The code an actor-roster read answers for an owner-only (unshared) set: it
/// has no roster, so its only writer is the owner — an empty roster, never a
/// failed read.
pub const ROSTER_NOT_SHARED: &str = "fauna.folders.not_shared";

/// What one pass over a served batch found — the counters the engine warns on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchVerdicts {
    pub verified: usize,
    pub exempt: usize,
    pub unverified: usize,
    pub refused: usize,
    /// The lowest seq held — the pull must not advance to or past it.
    pub held_from: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_writer_sig::{ChangeSigner, SignedChange};

    const NONCE: [u8; 32] = [7; 32];
    const OTHER_NONCE: [u8; 32] = [9; 32];

    fn kp(seed: u8) -> fauna_core::identity::ActorKeypair {
        fauna_core::identity::ActorKeypair::from_secret([seed; 32])
    }

    /// A row signed directly by `writer` under `nonce`, as the nest serves it.
    fn signed_row(writer: &fauna_core::identity::ActorKeypair, nonce: [u8; 32]) -> SyncChange {
        let mut row = SyncChange {
            seq: 5,
            path_hash: fauna_core::hex32::encode(&fauna_core::sync::path_hash("a.txt")),
            manifest_hash: Some(fauna_core::hex32::encode(&[3; 32])),
            size_bytes: 10,
            change_type: "create".into(),
            created_at: 1_000,
            device_id: Some(fauna_core::hex32::encode(&[4; 32])),
            author_actor_id: Some(writer.actor_id().to_hex()),
            path_sealed: Some(crate::ByteBuf::from(vec![1, 2, 3])),
            derived_through: Some(4),
            ..Default::default()
        };
        let signer = ChangeSigner::direct(writer);
        let statement = SignedChange::for_row(&row, nonce).unwrap();
        row.signature = Some(crate::ByteBuf::from(
            signer.sign_statement(&statement).to_vec(),
        ));
        row.signer_key = Some(crate::ByteBuf::from(signer.signer_key().to_vec()));
        row
    }

    fn reader(owner: &fauna_core::identity::ActorKeypair) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(owner.actor_id().0),
            ..Default::default()
        });
        r
    }

    #[test]
    fn the_owners_signed_row_verifies() {
        let owner = kp(1);
        let v = reader(&owner).judge(&signed_row(&owner, NONCE));
        assert!(
            matches!(v, RowVerdict::Verified { writer, signed_as, .. } if writer == owner.actor_id().0 && signed_as == writer)
        );
    }

    /// A projection that carries no statement is judged as an unsigned row:
    /// refused — never verified, never exempt.
    #[test]
    fn a_statement_less_projection_is_judged_as_unsigned() {
        assert_eq!(
            reader(&kp(1)).judge_projection(None),
            RowVerdict::Refused(ChangeVerifyError::Unsigned)
        );
        let owner = kp(1);
        assert!(matches!(
            reader(&owner).judge_projection(Some(&signed_row(&owner, NONCE))),
            RowVerdict::Verified { .. }
        ));
    }

    #[test]
    fn a_row_signed_under_another_sets_nonce_is_refused() {
        let owner = kp(1);
        let v = reader(&owner).judge(&signed_row(&owner, OTHER_NONCE));
        assert_eq!(v, RowVerdict::Refused(ChangeVerifyError::SignatureInvalid));
    }

    #[test]
    fn an_altered_signed_field_is_refused() {
        let owner = kp(1);
        let mut row = signed_row(&owner, NONCE);
        row.derived_through = Some(3);
        assert_eq!(
            reader(&owner).judge(&row),
            RowVerdict::Refused(ChangeVerifyError::SignatureInvalid)
        );
    }

    #[test]
    fn a_non_writer_member_is_refused_once_the_roster_was_read() {
        let (owner, member) = (kp(1), kp(2));
        let mut r = reader(&owner);
        r.install_roster(HashSet::new());
        assert_eq!(
            r.judge(&signed_row(&member, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
        r.install_roster(HashSet::from([member.actor_id().0]));
        assert!(matches!(
            r.judge(&signed_row(&member, NONCE)),
            RowVerdict::Verified { .. }
        ));
    }

    /// Ruling (8)(f)'s two refusals: only a row that verified under this
    /// set's nonce and resolves to no writer is one the reader cannot
    /// attribute *yet*; a row that fails cryptographically, an unsigned one,
    /// a held one and an admitted one are not.
    #[test]
    fn only_a_verified_row_by_no_writer_is_unattributed() {
        let (owner, member) = (kp(1), kp(2));
        let mut r = reader(&owner);
        assert!(!r.judge(&signed_row(&member, NONCE)).unattributed(), "held");
        r.install_roster(HashSet::new());
        assert!(r.judge(&signed_row(&member, NONCE)).unattributed());
        assert!(
            !r.judge(&signed_row(&member, OTHER_NONCE)).unattributed(),
            "another set's nonce: not a record"
        );
        let mut unsigned = signed_row(&member, NONCE);
        unsigned.signature = None;
        unsigned.signer_key = None;
        assert!(!r.judge(&unsigned).unattributed(), "unsigned: not a record");
        assert!(!r.judge(&signed_row(&owner, NONCE)).unattributed());
    }

    #[test]
    fn a_members_row_is_held_until_the_first_roster_read() {
        let (owner, member) = (kp(1), kp(2));
        assert_eq!(
            reader(&owner).judge(&signed_row(&member, NONCE)),
            RowVerdict::Held(Held::RosterUnread)
        );
    }

    /// A cross-nest member's set has a roster source (the relayed read,
    /// `federation.md` § Cross-nest…, *The cross-nest writer roster read*), so
    /// its non-owner rows take the same-nest shape: held until the first read,
    /// then judged by the roster it returned.
    #[test]
    fn a_cross_nest_members_row_is_held_until_the_first_roster_read_then_judged() {
        let (owner, writer, reader_member) = (kp(1), kp(2), kp(3));
        let mut r = reader(&owner);
        assert_eq!(
            r.judge(&signed_row(&writer, NONCE)),
            RowVerdict::Held(Held::RosterUnread),
            "never read ⇒ held, never admitted"
        );
        // The home nest's roster names the writer; the reader-access member is
        // not in it.
        r.install_roster(HashSet::from([writer.actor_id().0]));
        assert!(matches!(
            r.judge(&signed_row(&writer, NONCE)),
            RowVerdict::Verified { writer: w, .. } if w == writer.actor_id().0
        ));
        assert_eq!(
            r.judge(&signed_row(&reader_member, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
    }

    /// Every writer signs: an unsigned row outside the class exemptions is not
    /// a record.
    #[test]
    fn an_unsigned_row_is_refused() {
        let owner = kp(1);
        let mut row = signed_row(&owner, NONCE);
        row.signature = None;
        row.signer_key = None;
        let verdict = reader(&owner).judge(&row);
        assert_eq!(verdict, RowVerdict::Refused(ChangeVerifyError::Unsigned));
        assert!(!verdict.admits());
    }

    /// A signed row read before custody delivers the set's nonce cannot be
    /// verified yet: held (the pull retries), never admitted.
    #[test]
    fn no_nonce_holds_a_signed_row() {
        let owner = kp(1);
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            owner: Some(owner.actor_id().0),
            ..Default::default()
        });
        let row = signed_row(&owner, NONCE);
        let verdict = r.judge(&row);
        assert_eq!(verdict, RowVerdict::Held(Held::NoNonce));
        assert!(!verdict.admits());
    }

    /// Ruling (10)(e) — one row, two readers. The fold accounts and skips a
    /// retention row whatever it carries; a reader of versions judges it by
    /// its reporter's signature like any row: signed (flag covered) → verified
    /// as the reporter; unsigned → refused, never `Exempt`; signed as an
    /// ordinary row and served with the flag → refused.
    #[test]
    fn a_reader_of_versions_judges_a_retention_row_by_its_signature() {
        let owner = kp(1);
        let r = reader(&owner);
        let mut retained = signed_row(&owner, NONCE);
        retained.is_retention = Some(true);
        let statement = SignedChange::for_row(&retained, NONCE).unwrap();
        assert!(statement.is_retention, "the flag is in the statement");
        retained.signature = Some(crate::ByteBuf::from(
            ChangeSigner::direct(&owner)
                .sign_statement(&statement)
                .to_vec(),
        ));
        assert!(
            matches!(
                r.judge_projection(Some(&retained)),
                RowVerdict::Verified { writer, .. } if writer == owner.actor_id().0
            ),
            "a signed retention version is admitted as its reporter"
        );

        let unsigned = SyncChange {
            signature: None,
            signer_key: None,
            ..retained.clone()
        };
        assert_eq!(
            r.judge_projection(Some(&unsigned)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned),
            "an unsigned retention row is not a version"
        );

        let mut flagged = signed_row(&owner, NONCE);
        flagged.is_retention = Some(true);
        assert_eq!(
            r.judge_projection(Some(&flagged)),
            RowVerdict::Refused(ChangeVerifyError::SignatureInvalid),
            "an ordinary row served with the flag fails its statement"
        );

        for row in [&retained, &unsigned, &flagged] {
            assert_eq!(r.judge(row), RowVerdict::Exempt, "the fold keeps (3)");
        }
    }

    #[test]
    fn exempt_classes_skip_the_signature_check() {
        let owner = kp(1);
        let mut r = reader(&owner);
        let retention = SyncChange {
            is_retention: Some(true),
            ..Default::default()
        };
        assert_eq!(r.judge(&retention), RowVerdict::Exempt);
        // A served set's pseudo-device row is the nest's word by construction.
        let pseudo = SyncChange {
            device_id: Some(fauna_core::hex32::encode(
                &fauna_core::label_custody::webdav_pseudo_device_id(&owner.actor_id().0),
            )),
            ..Default::default()
        };
        assert_eq!(
            r.judge(&pseudo),
            RowVerdict::Refused(ChangeVerifyError::Unsigned),
            "not exempt while the set is unserved"
        );
        r.install_binding(ReaderBinding {
            webdav_served: true,
            ..r.binding().clone()
        });
        assert_eq!(r.judge(&pseudo), RowVerdict::Exempt);
    }

    /// Ruling (3)'s clarification: an item class routes a row away from the
    /// file reader, not past its check — a `state-entry` or `record-cid` row
    /// is no file row, signed or not, for the fold and a projection alike.
    #[test]
    fn an_item_class_row_is_no_file_row() {
        let owner = kp(1);
        let r = reader(&owner);
        use crate::account_state::ItemClass;
        for class in [ItemClass::StateEntry, ItemClass::RecordCid] {
            let forged = SyncChange {
                item_class: Some(class.as_wire().into()),
                manifest_hash: Some("05".repeat(32)),
                ..Default::default()
            };
            let mut signed = signed_row(&owner, NONCE);
            signed.item_class = Some(class.as_wire().into());
            for row in [&forged, &signed] {
                for verdict in [r.judge(row), r.judge_projection(Some(row))] {
                    assert_eq!(
                        verdict,
                        RowVerdict::Refused(ChangeVerifyError::OtherPlane),
                        "{class:?}"
                    );
                    assert!(!verdict.admits());
                    assert!(!verdict.wants_roster_read(), "the cursor passes it");
                }
            }
        }
    }

    #[test]
    fn a_plaintext_path_that_disagrees_with_the_signed_hash_is_refused() {
        let owner = kp(1);
        let mut row = signed_row(&owner, NONCE);
        row.path_sealed = None;
        // Re-sign without the sealed path, then serve a lying plaintext path.
        let signer = ChangeSigner::direct(&owner);
        row.signature = Some(crate::ByteBuf::from(
            signer
                .sign_statement(&SignedChange::for_row(&row, NONCE).unwrap())
                .to_vec(),
        ));
        row.path = Some("a.txt".into());
        assert!(matches!(
            reader(&owner).judge(&row),
            RowVerdict::Verified { .. }
        ));
        row.path = Some("b.txt".into());
        assert!(matches!(
            reader(&owner).judge(&row),
            RowVerdict::Refused(ChangeVerifyError::Malformed(_))
        ));
    }

    /// Ruling (2): a reader that places by a served plaintext `path` checks
    /// it — and the engine places by one whenever it is served, sealed label
    /// or not, so a sealed row served beside a lying plaintext path is
    /// refused too. An empty plaintext path is no path to place by.
    #[test]
    fn a_lying_plaintext_path_beside_a_sealed_label_is_refused() {
        let owner = kp(1);
        let mut row = signed_row(&owner, NONCE);
        assert!(row.path_sealed.is_some());
        row.path = Some(String::new());
        assert!(matches!(
            reader(&owner).judge(&row),
            RowVerdict::Verified { .. }
        ));
        row.path = Some("b.txt".into());
        assert!(matches!(
            reader(&owner).judge(&row),
            RowVerdict::Refused(ChangeVerifyError::Malformed(_))
        ));
    }
    // ── Ruling (8): rows signed under a retired identity ────────────────────

    use crate::folders::FolderActorMember;
    use fauna_core::identity::ActorKeypair;

    /// `old` succeeded by `new`, as the landed statement carries it: the
    /// verbatim canonical bytes, `new_sig` by `new_signer` (the successor
    /// itself for a genuine link).
    fn link(old: &ActorKeypair, new: &ActorKeypair, new_signer: &ActorKeypair) -> crate::ByteBuf {
        let recovery = fauna_core::recovery::RecoveryKey::generate();
        let signed = fauna_core::recovery::IdentitySuccession {
            old_actor_id: old.actor_id(),
            new_actor_id: new.actor_id(),
            recovery_pubkey: recovery.public(),
            seq: 2,
            created_at: fauna_core::data::Timestamp(0),
        }
        .sign(&recovery, new_signer.signing_key(), None)
        .expect("sign the succession");
        crate::ByteBuf::from(fauna_core::encoding::canonical_encode(&signed).expect("encode"))
    }

    fn member(
        who: &ActorKeypair,
        role: &str,
        access: Option<&str>,
        statements: Vec<crate::ByteBuf>,
    ) -> FolderActorMember {
        FolderActorMember {
            actor_id: who.actor_id().to_hex(),
            role: role.into(),
            access: access.map(str::to_string),
            succession_statements: statements,
            ..Default::default()
        }
    }

    /// A row `signer` signed directly, as the nest serves it after a
    /// succession moved its stamp to `served_author`.
    fn moved_row(signer: &ActorKeypair, served_author: &ActorKeypair) -> SyncChange {
        let mut row = signed_row(signer, NONCE);
        row.author_actor_id = Some(served_author.actor_id().to_hex());
        row
    }

    /// A row `device` signed under `root`'s `SyncWrite` cert, plus that cert —
    /// served with `served_author` as its stamp.
    fn delegated_row(
        root: &ActorKeypair,
        device: &ActorKeypair,
        served_author: &ActorKeypair,
    ) -> (SyncChange, EmbedAsBytes) {
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: root.actor_id(),
            device_key: device.actor_id().0,
            capabilities: vec![fauna_core::data::Capability::SyncWrite],
            created_at: fauna_core::data::Timestamp(0),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(root, &auth).unwrap();
        let cert = EmbedAsBytes::from_signed(bytes, env);
        let mut row = signed_row(root, NONCE);
        let signer = ChangeSigner::delegated(
            root.actor_id().0,
            ed25519_dalek::SigningKey::from_bytes(device.secret_bytes()),
            cert.clone(),
        );
        signer.sign_row(&mut row, NONCE).expect("signs");
        row.author_actor_id = Some(served_author.actor_id().to_hex());
        (row, cert)
    }

    /// The successor's reader over a set it owns: its own id, and the
    /// predecessor ids its account proved.
    fn successor_reader(successor: &ActorKeypair, predecessors: &[&ActorKeypair]) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(successor.actor_id().0),
            account: Some(successor.actor_id().0),
            account_predecessors: predecessors.iter().map(|k| k.actor_id().0).collect(),
            ..Default::default()
        });
        r
    }

    fn verified(
        writer: &ActorKeypair,
        signed_as: &ActorKeypair,
    ) -> (Option<[u8; 32]>, Option<[u8; 32]>) {
        (Some(writer.actor_id().0), Some(signed_as.actor_id().0))
    }

    fn named(v: &RowVerdict) -> (Option<[u8; 32]>, Option<[u8; 32]>) {
        match v {
            RowVerdict::Verified {
                writer, signed_as, ..
            } => (Some(*writer), Some(*signed_as)),
            _ => (None, None),
        }
    }

    /// The row this ruling exists for: the predecessor's DELEGATED row, served
    /// with the successor as author and the predecessor-named cert in the side
    /// table, verifies as written by the successor, signed as the predecessor.
    #[test]
    fn a_predecessors_delegated_row_verifies_as_its_successors() {
        let (pred, succ, device) = (kp(1), kp(2), kp(3));
        let (row, cert) = delegated_row(&pred, &device, &succ);
        let mut reader = successor_reader(&succ, &[&pred]);
        reader.ingest_certs([&cert]);
        let verdict = reader.judge(&row);
        assert_eq!(named(&verdict), verified(&succ, &pred), "{verdict:?}");
        assert!(matches!(
            verdict,
            RowVerdict::Verified { origin: AuthoringOrigin::Delegated { device_key }, .. }
                if device_key == device.actor_id().0
        ));
        // The cert is found under the identity that signed, never the writer.
        assert!(
            reader
                .cert_for(&pred.actor_id().0, &device.actor_id().0)
                .is_some()
        );
        assert!(
            reader
                .cert_for(&succ.actor_id().0, &device.actor_id().0)
                .is_none()
        );
        // Signed as a predecessor is never "signed as the current identity".
        assert!(!verdict.signed_as(Some(&succ.actor_id().0)));

        // A reader that never proved the link refuses the same row.
        let mut linkless = successor_reader(&succ, &[]);
        linkless.ingest_certs([&cert]);
        linkless.install_roster(HashSet::new());
        assert_eq!(
            linkless.judge(&row),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
    }

    /// The same for a predecessor's DIRECT signature (`signer_key` is the
    /// predecessor's actor id).
    #[test]
    fn a_predecessors_direct_row_verifies_as_its_successors() {
        let (pred, succ) = (kp(1), kp(2));
        let verdict = successor_reader(&succ, &[&pred]).judge(&moved_row(&pred, &succ));
        assert_eq!(named(&verdict), verified(&succ, &pred), "{verdict:?}");
        // The successor's own row is signed as the current identity.
        let own = successor_reader(&succ, &[&pred]).judge(&signed_row(&succ, NONCE));
        assert_eq!(named(&own), verified(&succ, &succ));
        assert!(own.signed_as(Some(&succ.actor_id().0)));
    }

    /// P0 → P → S: a P0-signed row is the account's own at any depth.
    #[test]
    fn a_two_hop_chain_admits_the_oldest_identitys_row() {
        let (p0, p, s) = (kp(1), kp(2), kp(3));
        let verdict = successor_reader(&s, &[&p, &p0]).judge(&moved_row(&p0, &s));
        assert_eq!(named(&verdict), verified(&s, &p0), "{verdict:?}");
        // The same proof carried on the roster's owner row, for a member.
        let mut member_reader = reader(&s);
        member_reader.install_roster(writer_roster(&[member(
            &s,
            "owner",
            None,
            vec![link(&p0, &p, &p), link(&p, &s, &s)],
        )]));
        let verdict = member_reader.judge(&moved_row(&p0, &s));
        assert_eq!(named(&verdict), verified(&s, &p0), "{verdict:?}");
    }

    /// A STRANGER's validly signed row — right nonce, its own cert — is still
    /// refused, whatever the nest stamps on it. The stamp is never an
    /// admission input.
    #[test]
    fn a_strangers_validly_signed_row_is_refused_whatever_the_stamp() {
        let (owner, pred, stranger, device) = (kp(1), kp(2), kp(8), kp(9));
        let mut r = successor_reader(&owner, &[&pred]);
        r.install_roster(HashSet::new());
        // Direct, stamped as itself; then stamped as the owner; then unstamped.
        assert_eq!(
            r.judge(&signed_row(&stranger, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
        assert_eq!(
            r.judge(&moved_row(&stranger, &owner)),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
        let mut unstamped = signed_row(&stranger, NONCE);
        unstamped.author_actor_id = None;
        assert_eq!(
            r.judge(&unstamped),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
        // Delegated under the stranger's own cert, stamped as the owner.
        let (row, cert) = delegated_row(&stranger, &device, &owner);
        r.ingest_certs([&cert]);
        assert_eq!(
            r.judge(&row),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter)
        );
    }

    /// A roster writer's predecessor is admitted only on a statement chain
    /// whose every `new_sig` verifies: no statements, a broken chain, a chain
    /// ending at a non-writer, and a link another key signed each prove
    /// nothing.
    #[test]
    fn a_roster_writers_predecessor_needs_a_verified_statement_chain() {
        let (owner, p0, p, w, reader_member, other) = (kp(1), kp(2), kp(3), kp(4), kp(5), kp(6));
        let row = moved_row(&p, &w);
        let judge = |members: &[FolderActorMember]| {
            let mut r = reader(&owner);
            r.install_roster(writer_roster(members));
            r.judge(&row)
        };
        let refused = RowVerdict::Refused(ChangeVerifyError::NotAWriter);

        // The genuine link: W's own signature naming P.
        let verdict = judge(&[member(&w, "member", Some("writer"), vec![link(&p, &w, &w)])]);
        assert_eq!(named(&verdict), verified(&w, &p), "{verdict:?}");

        // No statements at all — a writer with no proven history.
        assert_eq!(
            judge(&[member(&w, "member", Some("writer"), vec![])]),
            refused
        );
        // A broken chain: P0→P then OTHER→W.
        assert_eq!(
            judge(&[member(
                &w,
                "member",
                Some("writer"),
                vec![link(&p0, &p, &p), link(&other, &w, &w)],
            )]),
            refused
        );
        // A truncated chain that stops short of the writer.
        assert_eq!(
            judge(&[member(
                &w,
                "member",
                Some("writer"),
                vec![link(&p0, &p, &p)]
            )]),
            refused
        );
        // A chain ending at a NON-writer (a reader-access member).
        assert_eq!(
            judge(&[
                member(&w, "member", Some("writer"), vec![]),
                member(
                    &reader_member,
                    "member",
                    Some("reader"),
                    vec![link(&p, &reader_member, &reader_member)]
                ),
            ]),
            refused
        );
        // The nest's forgery: a link naming W as successor that another key
        // signed.
        assert_eq!(
            judge(&[member(
                &w,
                "member",
                Some("writer"),
                vec![link(&p, &w, &other)]
            )]),
            refused
        );
        // Undecodable statement bytes.
        assert_eq!(
            judge(&[member(
                &w,
                "member",
                Some("writer"),
                vec![crate::ByteBuf::from(vec![0xff; 12])],
            )]),
            refused
        );
        // A predecessor's row is HELD, never refused, before the first read.
        assert_eq!(
            reader(&owner).judge(&row),
            RowVerdict::Held(Held::RosterUnread)
        );
    }

    /// A lying `author_actor_id` changes no verdict's writer: the owner's row
    /// served as a member's, and a member's served as the owner's.
    #[test]
    fn a_lying_served_author_changes_no_verdicts_writer() {
        let (owner, writer) = (kp(1), kp(2));
        let mut r = reader(&owner);
        r.install_roster(HashSet::from([writer.actor_id().0]));
        let verdict = r.judge(&moved_row(&owner, &writer));
        assert_eq!(named(&verdict), verified(&owner, &owner), "{verdict:?}");
        let verdict = r.judge(&moved_row(&writer, &owner));
        assert_eq!(named(&verdict), verified(&writer, &writer), "{verdict:?}");
    }

    /// Precedence (ruling (8)(b)): an id that is itself a writer and is also
    /// claimed by another writer's chain is attributed to itself; an id two
    /// writers' chains both claim is attributed to itself, to neither.
    #[test]
    fn no_served_link_re_attributes_a_row() {
        let (owner, a, first, second) = (kp(1), kp(2), kp(3), kp(4));
        // A is a writer, and the first writer's (genuinely self-signed) chain claims it.
        let mut r = reader(&owner);
        r.install_roster(writer_roster(&[
            member(&a, "member", Some("writer"), vec![]),
            member(
                &first,
                "member",
                Some("writer"),
                vec![link(&a, &first, &first)],
            ),
        ]));
        let verdict = r.judge(&signed_row(&a, NONCE));
        assert_eq!(named(&verdict), verified(&a, &a), "{verdict:?}");

        // A is no writer, and two writers both claim it.
        let mut r = reader(&owner);
        r.install_roster(writer_roster(&[
            member(
                &first,
                "member",
                Some("writer"),
                vec![link(&a, &first, &first)],
            ),
            member(
                &second,
                "member",
                Some("writer"),
                vec![link(&a, &second, &second)],
            ),
        ]));
        let verdict = r.judge(&signed_row(&a, NONCE));
        assert_eq!(named(&verdict), verified(&a, &a), "{verdict:?}");
        assert!(verdict.admits());
    }

    /// What the reader admits by (ruling (8)(f)): the owner, the roster's
    /// writers and each one's proven predecessors — the own-account
    /// predecessors only while the account is itself a writer, and never a
    /// reader-access member or an unproven id.
    #[test]
    fn admitted_signers_are_the_writers_and_their_proven_predecessors() {
        let (pred, succ, w, w_pred, reader_member) = (kp(1), kp(2), kp(3), kp(4), kp(5));
        let ids = |keys: &[&ActorKeypair]| -> std::collections::BTreeSet<[u8; 32]> {
            keys.iter().map(|k| k.actor_id().0).collect()
        };
        // The owner's own reader: itself and its attested predecessor.
        let mut r = successor_reader(&succ, &[&pred]);
        assert_eq!(r.admitted_signers(), ids(&[&succ, &pred]));
        // A roster read adds each writer and what its chain proves.
        r.install_roster(writer_roster(&[
            member(&w, "member", Some("writer"), vec![link(&w_pred, &w, &w)]),
            member(&reader_member, "member", Some("reader"), vec![]),
        ]));
        assert_eq!(r.admitted_signers(), ids(&[&succ, &pred, &w, &w_pred]));
        // A member that is no writer of the set admits nothing by its own
        // predecessors.
        let mut member_reader = reader(&w);
        member_reader.install_binding(ReaderBinding {
            account: Some(succ.actor_id().0),
            account_predecessors: vec![pred.actor_id().0],
            ..member_reader.binding().clone()
        });
        member_reader.install_roster(HashSet::new());
        assert_eq!(member_reader.admitted_signers(), ids(&[&w]));
    }

    /// Ruling (8)(d): a served set's pseudo-device rows recorded before the
    /// succession carry the PREDECESSOR's pseudo-device — exempt on a served
    /// set, by either source of the link; never on an unserved one, and never
    /// for an unproven id.
    #[test]
    fn a_predecessors_webdav_pseudo_device_row_is_exempt_on_a_served_set() {
        let (pred, succ, stranger) = (kp(1), kp(2), kp(8));
        let pseudo = |of: &ActorKeypair| SyncChange {
            device_id: Some(fauna_core::hex32::encode(
                &fauna_core::label_custody::webdav_pseudo_device_id(&of.actor_id().0),
            )),
            ..Default::default()
        };
        let mut r = successor_reader(&succ, &[&pred]);
        assert_eq!(
            r.judge(&pseudo(&pred)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned),
            "not exempt while the set is unserved"
        );
        r.install_binding(ReaderBinding {
            webdav_served: true,
            ..r.binding().clone()
        });
        assert_eq!(r.judge(&pseudo(&pred)), RowVerdict::Exempt);
        assert_eq!(r.judge(&pseudo(&succ)), RowVerdict::Exempt);
        assert_eq!(
            r.judge(&pseudo(&stranger)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned)
        );
        // A member's reader takes the owner's predecessor from the roster's
        // owner row.
        let mut member_reader = reader(&succ);
        member_reader.install_binding(ReaderBinding {
            webdav_served: true,
            ..member_reader.binding().clone()
        });
        assert_eq!(
            member_reader.judge(&pseudo(&pred)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned)
        );
        member_reader.install_roster(writer_roster(&[member(
            &succ,
            "owner",
            None,
            vec![link(&pred, &succ, &succ)],
        )]));
        assert_eq!(member_reader.judge(&pseudo(&pred)), RowVerdict::Exempt);
    }

    // ── Ruling (11): the succession cut — three verdicts ────────────────────

    /// The successor `succ`'s reader over a set it owns whose live nonce
    /// [`NONCE`] `live_minter` minted, with [`OTHER_NONCE`] retired (minted by
    /// `retired_minter`), and `chain` as the owner's ordered predecessors.
    fn cut_reader(
        succ: &ActorKeypair,
        chain: &[&ActorKeypair],
        live_minter: Option<&ActorKeypair>,
        retired_minter: Option<&ActorKeypair>,
    ) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(succ.actor_id().0),
            account: Some(succ.actor_id().0),
            account_predecessors: chain.iter().map(|k| k.actor_id().0).collect(),
            live_minted_by: live_minter.map(|k| k.actor_id().0),
            retired_set_nonces: vec![(OTHER_NONCE, retired_minter.map(|k| k.actor_id().0))],
            owner_chain: chain.iter().map(|k| k.actor_id().0).collect(),
            ..Default::default()
        });
        r
    }

    /// Arm (1): under the live nonce, a row signed as a strict predecessor P
    /// of the owner is refused when the nonce's minter is a strict successor
    /// of P — the nonce was minted after P retired.
    #[test]
    fn a_predecessors_row_under_a_successor_minted_live_nonce_is_refused() {
        let (pred, succ) = (kp(1), kp(2));
        let r = cut_reader(&succ, &[&pred], Some(&succ), Some(&pred));
        assert_eq!(
            r.judge(&signed_row(&pred, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::HistoryEra)
        );
        // Two hops: P0 → P → W. The nonce P minted refuses P0's row, admits P's.
        let (p0, p, w) = (kp(5), kp(6), kp(7));
        let r = cut_reader(&w, &[&p, &p0], Some(&p), None);
        assert_eq!(
            r.judge(&signed_row(&p0, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::HistoryEra)
        );
        assert_eq!(named(&r.judge(&signed_row(&p, NONCE))), verified(&w, &p));
    }

    /// Arm (1)'s inheritance: a predecessor's row under a live nonce it minted
    /// itself, an earlier identity minted, or no recorded minter is current —
    /// ruling (8)'s un-cut state.
    #[test]
    fn a_predecessors_row_under_an_uncut_live_nonce_is_admitted() {
        let (pred, succ) = (kp(1), kp(2));
        for minter in [Some(&pred), None] {
            let r = cut_reader(&succ, &[&pred], minter, None);
            assert_eq!(
                named(&r.judge(&signed_row(&pred, NONCE))),
                verified(&succ, &pred)
            );
        }
        let (p0, p, w) = (kp(5), kp(6), kp(7));
        let r = cut_reader(&w, &[&p, &p0], Some(&p0), None);
        assert_eq!(named(&r.judge(&signed_row(&p, NONCE))), verified(&w, &p));
    }

    /// A minter the reader cannot place holds a predecessor's row (the roster
    /// is re-read), never the current owner's.
    #[test]
    fn an_unplaced_minter_holds_only_predecessor_signed_rows() {
        let (pred, succ, stranger) = (kp(1), kp(2), kp(8));
        let r = cut_reader(&succ, &[&pred], Some(&stranger), None);
        assert_eq!(
            r.judge(&signed_row(&pred, NONCE)),
            RowVerdict::Held(Held::MinterUnplaced)
        );
        assert_eq!(
            named(&r.judge(&signed_row(&succ, NONCE))),
            verified(&succ, &succ)
        );
    }

    /// Arm (2): under a retired nonce, a row signed as a strict predecessor of
    /// the owner is history — attributed as a verified row would be, never
    /// admitted.
    #[test]
    fn a_predecessors_row_under_a_retired_nonce_is_history() {
        let (pred, succ) = (kp(1), kp(2));
        let r = cut_reader(&succ, &[&pred], Some(&succ), Some(&pred));
        let verdict = r.judge(&signed_row(&pred, OTHER_NONCE));
        assert!(verdict.is_history() && !verdict.admits(), "{verdict:?}");
        assert!(matches!(
            verdict,
            RowVerdict::History { writer, signed_as, nonce, .. }
                if writer == succ.actor_id().0
                    && signed_as == pred.actor_id().0
                    && nonce == OTHER_NONCE
        ));
    }

    /// Arm (2) on a MOVED row — the shape the home nest serves after a
    /// succession moved the corpus: the served author is the successor, the
    /// signature is the predecessor's under a retired nonce. Under the live
    /// nonce the served author's candidate fails its chain, not its signature,
    /// and the row is still tried under the lineage: history, never refused.
    #[test]
    fn a_moved_predecessors_row_under_a_retired_nonce_is_history() {
        let (pred, succ) = (kp(1), kp(2));
        let r = cut_reader(&succ, &[&pred], Some(&succ), Some(&pred));
        let mut row = signed_row(&pred, OTHER_NONCE);
        row.author_actor_id = Some(succ.actor_id().to_hex());
        let verdict = r.judge(&row);
        assert!(
            matches!(
                verdict,
                RowVerdict::History { signed_as, nonce, .. }
                    if signed_as == pred.actor_id().0 && nonce == OTHER_NONCE
            ),
            "{verdict:?}"
        );
    }

    /// Arm (3): the current owner's rows and every row signed outside the
    /// owner's chain are current under a retired nonce as under the live one.
    #[test]
    fn the_owners_and_a_members_rows_under_a_retired_nonce_stay_current() {
        let (pred, succ, writer) = (kp(1), kp(2), kp(3));
        let mut r = cut_reader(&succ, &[&pred], Some(&succ), Some(&pred));
        r.install_roster(writer_roster(&[member(
            &writer,
            "member",
            Some("writer"),
            Vec::new(),
        )]));
        assert_eq!(
            named(&r.judge(&signed_row(&succ, OTHER_NONCE))),
            verified(&succ, &succ)
        );
        assert_eq!(
            named(&r.judge(&signed_row(&writer, OTHER_NONCE))),
            verified(&writer, &writer)
        );
        // A nonce outside the lineage verifies nothing.
        assert_eq!(
            r.judge(&signed_row(&writer, [0xEE; 32])),
            RowVerdict::Refused(ChangeVerifyError::SignatureInvalid)
        );
    }

    /// Membership in the chain is decided before ruling (8)(b)'s precedence: a
    /// roster that also lists the cut predecessor as a writer does not lift its
    /// rows out of arms (1) and (2).
    #[test]
    fn a_predecessor_the_roster_lists_as_a_writer_is_still_cut() {
        let (pred, succ) = (kp(1), kp(2));
        let mut r = cut_reader(&succ, &[&pred], Some(&succ), Some(&pred));
        r.install_roster(writer_roster(&[member(
            &pred,
            "member",
            Some("writer"),
            Vec::new(),
        )]));
        assert_eq!(
            r.judge(&signed_row(&pred, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::HistoryEra)
        );
        assert!(r.judge(&signed_row(&pred, OTHER_NONCE)).is_history());
    }

    /// A member's reader takes the owner's chain from the roster's owner row.
    #[test]
    fn a_members_reader_reads_the_chain_off_the_roster() {
        let (pred, succ) = (kp(1), kp(2));
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(succ.actor_id().0),
            live_minted_by: Some(succ.actor_id().0),
            retired_set_nonces: vec![(OTHER_NONCE, Some(pred.actor_id().0))],
            ..Default::default()
        });
        r.install_roster(writer_roster(&[member(
            &succ,
            "owner",
            None,
            vec![link(&pred, &succ, &succ)],
        )]));
        assert_eq!(
            r.judge(&signed_row(&pred, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::HistoryEra)
        );
        assert!(r.judge(&signed_row(&pred, OTHER_NONCE)).is_history());
    }

    /// Who names the owner (ruling (11)(c)): where the binding names one —
    /// the channel's MLS-recorded marker on a member's reader — the roster's
    /// owner row contributes that owner's statements and never the owner. A
    /// link is signed by its successor alone, so a roster whose owner row
    /// names `claimant` over a link P → claimant is the same bytes whether
    /// the claimant is P's real successor or any identity the nest lists: the
    /// reader cannot tell them apart, and deposing P on it would re-attribute
    /// P's rows to whoever claimed them. So P's rows stay P's own, current
    /// under the live nonce and under a retired one, whoever minted either.
    /// The binding is what must move — the marker re-points inside commit
    /// processing, and a nonce reaches a member only in an envelope that
    /// marker's owner signed — never the judge's reading of a served row.
    #[test]
    fn a_roster_owner_row_never_deposes_the_bindings_owner() {
        let (p, claimant) = (kp(1), kp(2));
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(p.actor_id().0),
            live_minted_by: Some(claimant.actor_id().0),
            retired_set_nonces: vec![(OTHER_NONCE, Some(p.actor_id().0))],
            ..Default::default()
        });
        r.install_roster(writer_roster(&[member(
            &claimant,
            "owner",
            None,
            vec![link(&p, &claimant, &claimant)],
        )]));
        assert_eq!(named(&r.judge(&signed_row(&p, NONCE))), verified(&p, &p));
        assert_eq!(
            named(&r.judge(&signed_row(&p, OTHER_NONCE))),
            verified(&p, &p)
        );
        // The row's writer is still a writer, as any roster writer is.
        assert_eq!(
            named(&r.judge(&signed_row(&claimant, NONCE))),
            verified(&claimant, &claimant)
        );
    }

    /// A reader whose binding names no owner — a host that holds no marker —
    /// over [`NONCE`] minted by `live_minter`, with [`OTHER_NONCE`] retired
    /// and minted by `retired_minter`.
    fn markerless_reader(live_minter: &ActorKeypair, retired_minter: &ActorKeypair) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            live_minted_by: Some(live_minter.actor_id().0),
            retired_set_nonces: vec![(OTHER_NONCE, Some(retired_minter.actor_id().0))],
            ..Default::default()
        });
        r
    }

    /// The marker-less half of ruling (11)(c): a reader whose binding names no
    /// owner reads it off the roster's owner row, so the cut's arms fire there
    /// as they do under a marker — a predecessor's row under a live nonce its
    /// successor minted is refused, one under a retired nonce is history.
    #[test]
    fn a_markerless_reader_takes_its_owner_off_the_rosters_owner_row() {
        let (pred, succ, writer) = (kp(1), kp(2), kp(3));
        let mut r = markerless_reader(&succ, &pred);
        // No owner and no roster: nothing is placed, everything holds.
        assert_eq!(
            r.judge(&signed_row(&succ, NONCE)),
            RowVerdict::Held(Held::RosterUnread)
        );
        r.install_roster(writer_roster(&[
            member(&succ, "owner", None, vec![link(&pred, &succ, &succ)]),
            member(&writer, "member", Some("writer"), Vec::new()),
        ]));
        assert_eq!(
            r.judge(&signed_row(&pred, NONCE)),
            RowVerdict::Refused(ChangeVerifyError::HistoryEra)
        );
        let history = r.judge(&signed_row(&pred, OTHER_NONCE));
        assert!(matches!(
            history,
            RowVerdict::History { writer, signed_as, .. }
                if writer == succ.actor_id().0 && signed_as == pred.actor_id().0
        ));
        // Arm (3) is untouched: the owner's and a member's rows are current
        // under either nonce.
        for nonce in [NONCE, OTHER_NONCE] {
            assert_eq!(
                named(&r.judge(&signed_row(&succ, nonce))),
                verified(&succ, &succ)
            );
            assert_eq!(
                named(&r.judge(&signed_row(&writer, nonce))),
                verified(&writer, &writer)
            );
        }
        assert!(r.admitted_signers().contains(&pred.actor_id().0));
    }

    /// The roster names an owner only when exactly one row carries the role:
    /// two owner rows name none (fail closed), and so does an undecodable id.
    #[test]
    fn a_roster_names_an_owner_only_from_exactly_one_owner_row() {
        let (a, b) = (kp(1), kp(2));
        assert_eq!(
            writer_roster(&[member(&a, "owner", None, Vec::new())]).owner,
            Some(a.actor_id().0)
        );
        assert_eq!(
            writer_roster(&[
                member(&a, "owner", None, Vec::new()),
                member(&b, "owner", None, Vec::new()),
            ])
            .owner,
            None
        );
        let mut undecodable = member(&a, "owner", None, Vec::new());
        undecodable.actor_id = "not-hex".into();
        assert_eq!(writer_roster(&[undecodable]).owner, None);
        assert_eq!(
            writer_roster(&[member(&b, "member", Some("writer"), Vec::new())]).owner,
            None
        );
    }

    /// A served set's pseudo-device row is unsigned by class, and exempt only
    /// as the owner's. A reader that holds no owner and has read no roster
    /// cannot tell yet, so the row holds for the one read that names the
    /// owner — refusing it would let the cursor pass a row the next read
    /// exempts.
    #[test]
    fn an_unsigned_row_holds_on_a_served_set_until_a_markerless_reader_has_an_owner() {
        let (pred, succ, stranger) = (kp(1), kp(2), kp(8));
        let pseudo = |of: &ActorKeypair| SyncChange {
            device_id: Some(fauna_core::hex32::encode(
                &fauna_core::label_custody::webdav_pseudo_device_id(&of.actor_id().0),
            )),
            ..Default::default()
        };
        let mut r = markerless_reader(&succ, &pred);
        assert_eq!(
            r.judge(&pseudo(&succ)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned),
            "an unserved set exempts nothing, owner or not"
        );
        r.install_binding(ReaderBinding {
            webdav_served: true,
            ..r.binding().clone()
        });
        let held = r.judge(&pseudo(&succ));
        assert_eq!(held, RowVerdict::Held(Held::RosterUnread));
        assert!(held.wants_roster_read());
        assert_eq!(
            r.judge(&SyncChange::default()),
            RowVerdict::Refused(ChangeVerifyError::Unsigned),
            "a row naming no device is no pseudo-device row"
        );
        r.install_roster(writer_roster(&[member(
            &succ,
            "owner",
            None,
            vec![link(&pred, &succ, &succ)],
        )]));
        assert_eq!(r.judge(&pseudo(&succ)), RowVerdict::Exempt);
        assert_eq!(r.judge(&pseudo(&pred)), RowVerdict::Exempt);
        assert_eq!(
            r.judge(&pseudo(&stranger)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned)
        );
        // A roster that names no owner ends the hold: nothing more to wait on.
        let mut ownerless = markerless_reader(&succ, &pred);
        ownerless.install_binding(ReaderBinding {
            webdav_served: true,
            ..ownerless.binding().clone()
        });
        ownerless.install_roster(HashSet::new());
        assert_eq!(
            ownerless.judge(&pseudo(&succ)),
            RowVerdict::Refused(ChangeVerifyError::Unsigned)
        );
    }
}

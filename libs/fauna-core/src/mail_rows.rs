//! The `fauna.state.mail` plane rows — the key grammar, the state row born
//! for the plane, the per-row joins and the READ fold
//! (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The mail
//! plane* owns the ruling; `mail-credentials.md` § Rotation and recovery →
//! *The generation marker* owns the marker's invariant).
//!
//! **Three row families, the key's first segment dispatching:**
//!
//! | key | value |
//! |---|---|
//! | `self` | the account's ONE [`MailStateRow`] — the MSEK, the succession burns, the rotation sentinel, the three flags, the row's own stamp |
//! | `credential/<credential_id>` | one [`MailCredential`], the value naming its key |
//! | `generation/<fingerprint>` | one retired MSEK generation — a [`PriorMsekRetirement`] (the MSEK and its retirement instant), keyed by its [`MsekFingerprint`] in lowercase hex |
//!
//! The credentials are one row each for the atproto reason: the list grows by
//! one per MUA with no count cap, every row carries a secret, and a burned
//! row is kept on purpose — a `self` row holding them would be bounded by
//! use, not by shape. The generations are one row each for the same reason:
//! every MSEK ever retired is kept, UNCAPPED, so a record sealed to any
//! generation still opens (`owner-key-material.md` § Path B-sibling-2 →
//! *Pre-rotation mail at rest*) — the list grows by one per rotation
//! *ceremony*. Everything else is fixed by shape except the burns, which grow
//! by one per succession ceremony (the size pin seals a 512-burn row under
//! half the cap).
//!
//! **The joins.** [`MailStateRow::merge`] runs the MSEK and burn halves
//! [`MailConfig::merge`] runs — the same functions — on the MSEK's own stamp
//! (`msek_at`), the sentinel latest-wins on ITS own stamp
//! (`pending_rotation_at`), then the three flags as ONE latest-wins record on
//! `updated_at`, `mail_enabled` present-wins; each stamp advances only on a
//! write that changes its half ([`MailStateRow::written_over`], the door's
//! rule), and the sentinel is cleared only by a clear naming its
//! MSEK′ ([`MailStateRow::rotation_cleared`]). [`MailConfig::merge`] decides
//! the recreatable FIVE (the credential list and the sentinel ride with the
//! flags there) on its caller-supplied record instants, the one ruled
//! divergence between the two. [`MailCredential::merge`] ORs the two monotone markers and takes
//! the remainder on the row's stamp. A generation row is immutable once
//! written: its MSEK is its key's preimage, and two devices recording one
//! retirement join on the LATER instant ([`merge_generation`]). There is no
//! deletion: a revoke and a burn are markers, a marked id is **spent**
//! ([`MailRows::spent_credential_ids`]), and a generation is never dropped.
//!
//! **Decode posture.** [`MailStateRow`] is `deny_unknown_fields` (it has no
//! whole-record twin); [`MailCredential`] and [`PriorMsekRetirement`] stay
//! tolerant (they are shared with [`MailConfig`]), so every family is ALSO
//! decoded strict by round-trip — a row must re-encode to exactly its own
//! bytes, which a value carrying a field this build does not know (at any
//! depth) or any non-canonical spelling cannot. A marked credential row
//! carrying a secret is refused too: the join never produces one. So is a
//! generation row whose MSEK does not fingerprint to its key.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::data::{
    MailConfig, MailCredential, MailSuccessionBurn, MsekFingerprint, PendingRotation,
    PriorMsekRetirement, Timestamp, merge_msek, merge_prior_generations, merge_succession_burns,
};
use crate::error::{Error, Result};
use crate::secret::SecretArray32;

/// The key of the account's one mail-state row.
pub const STATE_KEY: &str = "self";
/// Key prefix of a credential row: `credential/<credential_id>`.
pub const CREDENTIAL_PREFIX: &str = "credential/";
/// Key prefix of a retired-generation row: `generation/<fingerprint>`, the
/// [`MsekFingerprint`] as 64 lowercase hex digits.
pub const GENERATION_PREFIX: &str = "generation/";

/// A parsed `fauna.state.mail` row key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailRowKey {
    /// `self` — the [`MailStateRow`].
    State,
    /// `credential/<credential_id>` — one [`MailCredential`].
    Credential(String),
    /// `generation/<fingerprint>` — one retired MSEK generation.
    Generation(MsekFingerprint),
}

impl MailRowKey {
    /// Parse a row key.
    ///
    /// # Errors
    /// A key outside the grammar, a credential key with an empty id, or a
    /// generation key that is not 64 lowercase hex digits.
    pub fn parse(key: &str) -> Result<Self> {
        if key == STATE_KEY {
            return Ok(Self::State);
        }
        if let Some(hex_fp) = key.strip_prefix(GENERATION_PREFIX) {
            let mut fp = [0u8; 32];
            // Lowercase only: one generation, one key spelling.
            if !hex_fp.bytes().any(|b| b.is_ascii_uppercase())
                && hex::decode_to_slice(hex_fp, &mut fp).is_ok()
            {
                return Ok(Self::Generation(MsekFingerprint(fp)));
            }
            return Err(Error::Encoding(format!("not a mail row key: {key:?}")));
        }
        match key.strip_prefix(CREDENTIAL_PREFIX) {
            Some(id) if !id.is_empty() => Ok(Self::Credential(id.to_string())),
            _ => Err(Error::Encoding(format!("not a mail row key: {key:?}"))),
        }
    }

    /// The key string.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::State => STATE_KEY.to_string(),
            Self::Credential(id) => format!("{CREDENTIAL_PREFIX}{id}"),
            Self::Generation(fp) => format!("{GENERATION_PREFIX}{}", hex::encode(fp.0)),
        }
    }
}

/// **The generation row's join**: one MSEK (the key's preimage, so both sides
/// carry the same one), the LATER of two recorded retirement instants. Two
/// devices retiring one generation in a cross-device finalize race each record
/// their own instant; the instant bounds the generation's seal interval for
/// the bounded-mail mint and orders the openers' trial, so extending it is the
/// no-data-loss direction, and `max` is symmetric where first-recorded is not
/// (the rule the retired capped window's retirement map ran, kept).
#[must_use]
pub fn merge_generation(
    ours: &PriorMsekRetirement,
    theirs: &PriorMsekRetirement,
) -> PriorMsekRetirement {
    PriorMsekRetirement {
        msek: ours.msek.clone(),
        retired_at_unix: ours.retired_at_unix.max(theirs.retired_at_unix),
    }
}

/// The plane's rotation sentinel: the incoming MSEK **alone**. The set of
/// credentials still owed a re-wrap is derived from the rows' generation
/// markers ([`MailRows::owed_rewrap`]), never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailRotationSentinel {
    /// The incoming MSEK, in custody across the resumable swap.
    pub new_msek: SecretArray32,
}

/// **The account's one mail-state row** (`self`), born for the plane: the
/// fields of [`MailConfig`] that are fixed by shape, plus one stamp per
/// independently written half (P3). `deny_unknown_fields` — no blob carries it.
///
/// **Three stamps, one per half:** the MSEK's (`msek_at`), the
/// sentinel's (`pending_rotation_at`) and the flags' (`updated_at`). Each
/// advances only on a write that CHANGES its half ([`Self::written_over`]), so
/// a write made from a replica that has not yet merged another device's swap
/// or sentinel restates that half at its STORED stamp — it can neither outrank
/// the swap nor clear the sentinel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailStateRow {
    /// [`MailConfig::msek`]. The generations it displaced live in their own
    /// `generation/<fingerprint>` rows, never here.
    pub msek: Option<SecretArray32>,
    /// The key material's own stamp — what the MSEK rotation pick compares;
    /// only a write that changes `msek` (a swap or a mint) advances it.
    pub msek_at: Timestamp,
    /// [`MailConfig::succession_burns`] — one per succession ceremony.
    pub succession_burns: Vec<MailSuccessionBurn>,
    /// The rotation sentinel ([`MailRotationSentinel`]).
    pub pending_rotation: Option<MailRotationSentinel>,
    /// The sentinel's own stamp — what its latest-wins decision compares; only
    /// a write that sets or replaces the sentinel, or the explicit clear naming
    /// its MSEK′ ([`Self::rotation_cleared`]), advances it.
    pub pending_rotation_at: Timestamp,
    /// [`MailConfig::mail_enabled`].
    pub mail_enabled: Option<bool>,
    /// [`MailConfig::caldav_enabled`].
    pub caldav_enabled: bool,
    /// [`MailConfig::carddav_enabled`].
    pub carddav_enabled: bool,
    /// The flags' stamp — what the three flags' latest-wins decision compares.
    pub updated_at: Timestamp,
}

impl MailStateRow {
    /// **The state row's join** (`config-dissolution.md` § *The mail plane*):
    /// the MSEK present-wins / rotation-LWW on `msek_at` and the burn
    /// min-union — the functions [`MailConfig::merge`] runs; the sentinel
    /// latest-wins on `pending_rotation_at`; the three flags as ONE
    /// latest-wins decision on `updated_at`, `mail_enabled` present-wins.
    /// Each joined stamp is its half's winner's.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let msek = merge_msek(&self.msek, self.msek_at, &other.msek, other.msek_at);
        // The winning key's own stamp: present-wins carries the present side's,
        // and a differing pair's winner holds the later stamp, so `max` is its.
        let msek_at = match (&self.msek, &other.msek) {
            (Some(_), None) => self.msek_at,
            (None, Some(_)) => other.msek_at,
            _ => self.msek_at.max(other.msek_at),
        };
        let succession_burns =
            merge_succession_burns(&self.succession_burns, &other.succession_burns);
        let pending_rotation = if crate::latest_wins::theirs_wins(
            &self.pending_rotation,
            self.pending_rotation_at,
            &other.pending_rotation,
            other.pending_rotation_at,
        ) {
            other.pending_rotation.clone()
        } else {
            self.pending_rotation.clone()
        };
        let (win, lose) = if crate::latest_wins::theirs_wins(
            &(self.mail_enabled, self.caldav_enabled, self.carddav_enabled),
            self.updated_at,
            &(
                other.mail_enabled,
                other.caldav_enabled,
                other.carddav_enabled,
            ),
            other.updated_at,
        ) {
            (other, self)
        } else {
            (self, other)
        };
        Self {
            msek,
            msek_at,
            succession_burns,
            pending_rotation,
            pending_rotation_at: self.pending_rotation_at.max(other.pending_rotation_at),
            // Present-wins, the winner's own value first ([`MailConfig::merge`]'s rule).
            mail_enabled: win.mail_enabled.or(lose.mail_enabled),
            caldav_enabled: win.caldav_enabled,
            carddav_enabled: win.carddav_enabled,
            updated_at: self.updated_at.max(other.updated_at),
        }
    }

    /// The state half of a [`MailConfig`], every half stamped `at` (its prior
    /// generations are [`MailConfig::generation_rows`]).
    #[must_use]
    pub fn from_config(config: &MailConfig, at: Timestamp) -> Self {
        Self {
            msek: config.msek.clone(),
            msek_at: at,
            succession_burns: config.succession_burns.clone(),
            pending_rotation: config
                .pending_rotation
                .as_ref()
                .map(|p| MailRotationSentinel {
                    new_msek: p.new_msek.clone(),
                }),
            pending_rotation_at: at,
            mail_enabled: config.mail_enabled,
            caldav_enabled: config.caldav_enabled,
            carddav_enabled: config.carddav_enabled,
            updated_at: at,
        }
    }

    /// **A write of the flags alone** — the three flags and `mail_enabled` as
    /// `config` holds them, with the key material AND the rotation sentinel
    /// left OUT: `msek` absent, the burns empty, `pending_rotation` absent. The
    /// shape every flag write takes; [`Self::written_over`] reads an absent
    /// MSEK or sentinel as no change, so a flag flip made from a replica that
    /// has not yet seen another device's rotation can never revert the MSEK it
    /// swapped in nor clear its sentinel, and one made from a load that
    /// predates a clear can never set that sentinel again above it
    /// (`mail-credentials.md` § Cross-device finalize race). A write that sets
    /// a sentinel or an MSEK names it explicitly over this base.
    #[must_use]
    pub fn recreatable_of(config: &MailConfig) -> Self {
        Self {
            msek: None,
            succession_burns: Vec::new(),
            pending_rotation: None,
            ..Self::from_config(config, Timestamp::default())
        }
    }

    /// The latest of the row's three stamps — what a new write's stamp must
    /// order after.
    #[must_use]
    pub fn latest_stamp(&self) -> Timestamp {
        self.msek_at
            .max(self.pending_rotation_at)
            .max(self.updated_at)
    }

    /// **The door's write rule**: this intent written over `stored`
    /// at `at` (strictly above `stored`'s [`Self::latest_stamp`]); the
    /// intent's own stamps are ignored. Each half takes `at` only when the
    /// intent CHANGES it and otherwise keeps the stored value at its stored
    /// stamp: an absent MSEK or sentinel is no change (neither is ever cleared
    /// here — the sentinel's clear is [`Self::rotation_cleared`]), nor is a
    /// restatement of the stored value. So a write from a replica that has not
    /// yet merged another device's committed swap or sentinel carries that
    /// replica's stored half at its OLD stamp, which the join ranks below the
    /// swap and the sentinel. `mail_enabled` absent keeps the stored flag; the
    /// burns union.
    #[must_use]
    pub fn written_over(&self, stored: Option<&Self>, at: Timestamp) -> Self {
        let base = stored.cloned().unwrap_or_default();
        let (msek, msek_at) = match &self.msek {
            Some(k) if base.msek.as_ref() != Some(k) => (Some(k.clone()), at),
            _ => (base.msek.clone(), base.msek_at),
        };
        let (pending_rotation, pending_rotation_at) = match &self.pending_rotation {
            Some(s) if base.pending_rotation.as_ref() != Some(s) => (Some(s.clone()), at),
            _ => (base.pending_rotation.clone(), base.pending_rotation_at),
        };
        let flags = (
            self.mail_enabled.or(base.mail_enabled),
            self.caldav_enabled,
            self.carddav_enabled,
        );
        let updated_at = if flags == (base.mail_enabled, base.caldav_enabled, base.carddav_enabled)
        {
            base.updated_at
        } else {
            at
        };
        let (mail_enabled, caldav_enabled, carddav_enabled) = flags;
        Self {
            msek,
            msek_at,
            succession_burns: merge_succession_burns(
                &base.succession_burns,
                &self.succession_burns,
            ),
            pending_rotation,
            pending_rotation_at,
            mail_enabled,
            caldav_enabled,
            carddav_enabled,
            updated_at,
        }
    }

    /// **The sentinel's explicit clear**: `stored` with its sentinel cleared at
    /// `at` (strictly above `stored`'s [`Self::latest_stamp`]) — only when the
    /// stored sentinel names `new_msek`. A clear naming a different MSEK′, or
    /// over no sentinel, is no clear (`None`): the clearing writer must have
    /// SEEN the sentinel it clears.
    #[must_use]
    pub fn rotation_cleared(
        stored: &Self,
        new_msek: &SecretArray32,
        at: Timestamp,
    ) -> Option<Self> {
        let names_it = stored
            .pending_rotation
            .as_ref()
            .is_some_and(|s| s.new_msek == *new_msek);
        names_it.then(|| Self {
            pending_rotation: None,
            pending_rotation_at: at,
            ..stored.clone()
        })
    }
}

/// One `fauna.state.mail` row's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailRecord {
    /// The `self` row.
    State(MailStateRow),
    /// A `credential/<credential_id>` row.
    Credential(MailCredential),
    /// A `generation/<fingerprint>` row.
    Generation(PriorMsekRetirement),
}

impl MailRecord {
    /// The key this value lives at.
    ///
    /// # Errors
    /// A credential with an empty id.
    pub fn plane_key(&self) -> Result<String> {
        match self {
            Self::State(_) => Ok(STATE_KEY.to_string()),
            Self::Credential(c) if c.credential_id.is_empty() => Err(Error::Encoding(
                "a mail credential needs a credential_id to key its row".into(),
            )),
            Self::Credential(c) => Ok(MailRowKey::Credential(c.credential_id.clone()).key()),
            Self::Generation(g) => Ok(MailRowKey::Generation(MsekFingerprint::of(&g.msek)).key()),
        }
    }

    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::State(s) => crate::encoding::canonical_encode(s),
            Self::Credential(c) => crate::encoding::canonical_encode(c),
            Self::Generation(g) => crate::encoding::canonical_encode(g),
        }
    }

    /// The per-row join: [`MailStateRow::merge`], [`MailCredential::merge`] or
    /// [`merge_generation`].
    ///
    /// # Errors
    /// The two sides are different rows.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::State(a), Self::State(b)) => Ok(Self::State(a.merge(b))),
            (Self::Credential(a), Self::Credential(b)) if a.credential_id == b.credential_id => {
                Ok(Self::Credential(a.merge(b)))
            }
            (Self::Generation(a), Self::Generation(b)) if a.msek == b.msek => {
                Ok(Self::Generation(merge_generation(a, b)))
            }
            _ => Err(Error::Encoding("mail rows name different rows".into())),
        }
    }
}

/// Decode `value` as `T`, refusing bytes that do not re-encode to themselves
/// (the module doc's posture).
fn decode_exact<T>(value: &[u8]) -> Result<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let decoded: T = crate::encoding::canonical_decode(value)?;
    if crate::encoding::canonical_encode(&decoded)? != value {
        return Err(Error::Encoding(
            "mail row carries a field this build does not know".into(),
        ));
    }
    Ok(decoded)
}

/// Decode one `fauna.state.mail` row: the key's first segment picks the
/// family, the value must decode as it exactly (the module doc's posture), a
/// credential must name its key's id, a marked credential must carry no
/// secret, and a generation's MSEK must fingerprint to its key.
///
/// # Errors
/// A key outside the grammar, an undecodable or inexact value, a misfiled
/// credential or generation, or a marked credential still holding a secret.
pub fn decode_mail_row(key: &str, value: &[u8]) -> Result<MailRecord> {
    match MailRowKey::parse(key)? {
        MailRowKey::State => Ok(MailRecord::State(decode_exact(value)?)),
        MailRowKey::Credential(id) => {
            let c: MailCredential = decode_exact(value)?;
            if c.credential_id != id {
                return Err(Error::Encoding(format!(
                    "mail credential row at {key:?} names {:?}",
                    c.credential_id
                )));
            }
            if c.is_marked() && !c.secret.is_empty() {
                return Err(Error::Encoding(format!(
                    "marked mail credential {key:?} still carries a secret"
                )));
            }
            Ok(MailRecord::Credential(c))
        }
        MailRowKey::Generation(fp) => {
            let g: PriorMsekRetirement = decode_exact(value)?;
            if MsekFingerprint::of(&g.msek) != fp {
                return Err(Error::Encoding(format!(
                    "mail generation row at {key:?} holds another generation"
                )));
            }
            Ok(MailRecord::Generation(g))
        }
    }
}

/// The account's mail rows, decoded and joined per key — the fold's working
/// set, which keeps the revoked rows [`MailConfig`] hides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailRows {
    /// The `self` row, if one rests.
    pub state: Option<MailStateRow>,
    /// Every credential row, revoked included, by id.
    pub credentials: BTreeMap<String, MailCredential>,
    /// Every retired MSEK generation, by fingerprint — uncapped.
    pub generations: BTreeMap<MsekFingerprint, PriorMsekRetirement>,
}

impl MailRows {
    /// Decode and join one row into the set (a second row at one key joins,
    /// so the fold is order-independent).
    ///
    /// # Errors
    /// [`decode_mail_row`]'s refusals.
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        match decode_mail_row(key, value)? {
            MailRecord::State(s) => {
                self.state = Some(match self.state.take() {
                    Some(cur) => cur.merge(&s),
                    None => s,
                });
            }
            MailRecord::Credential(c) => {
                let joined = match self.credentials.get(&c.credential_id) {
                    Some(cur) => cur.merge(&c),
                    None => c,
                };
                self.credentials
                    .insert(joined.credential_id.clone(), joined);
            }
            MailRecord::Generation(g) => self.join_generation(g),
        }
        Ok(())
    }

    /// Join one retired generation into the set ([`merge_generation`]).
    pub fn join_generation(&mut self, g: PriorMsekRetirement) {
        let fp = MsekFingerprint::of(&g.msek);
        let joined = match self.generations.get(&fp) {
            Some(cur) => merge_generation(cur, &g),
            None => g,
        };
        self.generations.insert(fp, joined);
    }

    /// Fold every `(key, value)` row.
    ///
    /// # Errors
    /// [`decode_mail_row`]'s refusals.
    pub fn from_rows<'a>(rows: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Self> {
        let mut out = Self::default();
        for (k, v) in rows {
            out.fold_row(k, v)?;
        }
        Ok(out)
    }

    /// **Every id the plane holds, revoked included** — the set
    /// `derive_credential_id` must dedup against: a marked id is spent, since
    /// a re-mint under it would join as revoked.
    #[must_use]
    pub fn spent_credential_ids(&self) -> Vec<String> {
        self.credentials.keys().cloned().collect()
    }

    /// The live rows owed a re-wrap under `msek` — neither burned nor revoked,
    /// and not naming `msek`'s fingerprint (`None` is owed) — oldest first
    /// (*The generation marker*).
    #[must_use]
    pub fn owed_rewrap(&self, msek: &SecretArray32) -> Vec<&MailCredential> {
        let fp = MsekFingerprint::of(msek);
        let mut owed: Vec<&MailCredential> = self
            .credentials
            .values()
            .filter(|c| !c.is_marked() && c.wrapped_under != Some(fp))
            .collect();
        owed.sort_by(|a, b| {
            (a.created_at, &a.credential_id).cmp(&(b.created_at, &b.credential_id))
        });
        owed
    }

    /// The live rows a device holding the plane owes a **heal** — a re-wrap
    /// under the current `msek` (*The generation marker*): every live row not
    /// naming its fingerprint, `None` included, while no rotation is pending
    /// (a pending rotation owns its own owed set, against its incoming MSEK).
    /// A replica whose state row lags its credential rows may re-wrap a row
    /// under an MSEK another device has since rotated away from; that is
    /// convergent, not lossy — the row then names the older generation, which
    /// every current replica reads as owed and re-wraps on its next pass.
    /// Oldest first.
    #[must_use]
    pub fn heal_owed(&self) -> Vec<&MailCredential> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        let Some(msek) = &state.msek else {
            return Vec::new();
        };
        if state.pending_rotation.is_some() {
            return Vec::new();
        }
        self.owed_rewrap(msek)
    }

    /// The **dual of the heal**: marked rows (burned or revoked) that still
    /// name a generation — their nest-side blobs may still rest (a racing
    /// re-wrap re-provisioned them, or the marking device stopped between the
    /// mark and its deletes), so both revoke calls are owed again, then the
    /// marker written. Oldest first.
    #[must_use]
    pub fn delete_owed(&self) -> Vec<&MailCredential> {
        let mut owed: Vec<&MailCredential> = self
            .credentials
            .values()
            .filter(|c| c.is_marked() && c.wrapped_under.is_some())
            .collect();
        owed.sort_by(|a, b| {
            (a.created_at, &a.credential_id).cmp(&(b.created_at, &b.credential_id))
        });
        owed
    }

    /// **The READ fold** — the composite [`MailConfig`]: the state row's
    /// fields, every retired generation as `prior_mseks` +
    /// `prior_msek_retirements` (most recently retired first, ties by key
    /// bytes, the current MSEK excluded — UNCAPPED), plus every credential row
    /// that is not revoked (burned rows SHOWN, they are the user's list to
    /// re-add), oldest first by `created_at` then id.
    #[must_use]
    pub fn config(&self) -> MailConfig {
        let state = self.state.clone().unwrap_or_default();
        let mut credentials: Vec<MailCredential> = self
            .credentials
            .values()
            .filter(|c| c.revoked_at_unix.is_none())
            .cloned()
            .collect();
        credentials.sort_by(|a, b| {
            (a.created_at, &a.credential_id).cmp(&(b.created_at, &b.credential_id))
        });
        let pending_rotation = state.pending_rotation.as_ref().map(|s| PendingRotation {
            new_msek: s.new_msek.clone(),
        });
        let retirements: Vec<PriorMsekRetirement> = self.generations.values().cloned().collect();
        let mseks: Vec<SecretArray32> = retirements.iter().map(|g| g.msek.clone()).collect();
        let (prior_mseks, prior_msek_retirements) =
            merge_prior_generations(state.msek.as_ref(), (&mseks, &retirements), (&[], &[]));
        MailConfig {
            msek: state.msek,
            prior_mseks,
            credentials,
            pending_rotation,
            mail_enabled: state.mail_enabled,
            caldav_enabled: state.caldav_enabled,
            carddav_enabled: state.carddav_enabled,
            prior_msek_retirements,
            succession_burns: state.succession_burns,
        }
    }
}

impl MailConfig {
    /// Every plane row of this composite, `(key, record)`: the state row
    /// stamped `state_at`, then each credential in list order, then each prior
    /// generation ([`Self::generation_rows`]).
    ///
    /// # Errors
    /// A credential with an empty id, or a prior generation with no recorded
    /// retirement instant.
    pub fn rows(&self, state_at: Timestamp) -> Result<Vec<(String, MailRecord)>> {
        std::iter::once(MailRecord::State(MailStateRow::from_config(self, state_at)))
            .chain(self.credentials.iter().cloned().map(MailRecord::Credential))
            .chain(
                self.generation_rows()?
                    .into_iter()
                    .map(MailRecord::Generation),
            )
            .map(|r| Ok((r.plane_key()?, r)))
            .collect()
    }

    /// Each prior generation as its `generation/<fingerprint>` row's value, in
    /// `prior_mseks` order.
    ///
    /// # Errors
    /// A prior generation with no recorded retirement instant — an
    /// inconsistent composite (the fold never produces one).
    pub fn generation_rows(&self) -> Result<Vec<PriorMsekRetirement>> {
        self.prior_mseks
            .iter()
            .map(|k| {
                self.prior_msek_retired_at(k)
                    .map(|retired_at_unix| PriorMsekRetirement {
                        msek: k.clone(),
                        retired_at_unix,
                    })
                    .ok_or_else(|| {
                        Error::Encoding("a prior mail generation has no retirement instant".into())
                    })
            })
            .collect()
    }

    /// **The READ fold** over the account's rows ([`MailRows::config`]).
    ///
    /// # Errors
    /// [`decode_mail_row`]'s refusals.
    pub fn fold<'a>(rows: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Self> {
        Ok(MailRows::from_rows(rows)?.config())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::MailCredentialKind;
    use crate::identity::ActorId;

    fn cred(id: &str, created: u64, at: u64) -> MailCredential {
        MailCredential {
            credential_id: id.into(),
            display_name: format!("{id} name"),
            kind: MailCredentialKind::Plain,
            secret: vec![7; 8].into(),
            created_at: created,
            updated_at: Timestamp(at),
            wrapped_under: None,
            token_minted_at: None,
            revoked_at_unix: None,
            burned: None,
        }
    }

    fn sample() -> MailConfig {
        MailConfig {
            msek: Some(SecretArray32::new([1; 32])),
            prior_mseks: vec![SecretArray32::new([2; 32])],
            credentials: vec![cred("b-mail", 10, 5), cred("a-mail", 20, 5)],
            pending_rotation: None,
            mail_enabled: Some(true),
            caldav_enabled: true,
            carddav_enabled: false,
            prior_msek_retirements: vec![PriorMsekRetirement {
                msek: SecretArray32::new([2; 32]),
                retired_at_unix: 100,
            }],
            succession_burns: vec![MailSuccessionBurn {
                predecessor: ActorId([9; 32]),
                at_unix: 50,
            }],
        }
    }

    /// Split and fold are inverses, the keys are the documented grammar, and
    /// the fold orders credentials oldest first whatever the row order.
    #[test]
    fn rows_round_trip_through_the_fold() {
        let cfg = sample();
        let rows = cfg.rows(Timestamp(1)).unwrap();
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        let gen_key = format!(
            "generation/{}",
            hex::encode(MsekFingerprint::of(&SecretArray32::new([2; 32])).0)
        );
        assert_eq!(
            keys,
            [
                "self",
                "credential/b-mail",
                "credential/a-mail",
                gen_key.as_str()
            ]
        );
        let enc: Vec<(String, Vec<u8>)> = rows
            .iter()
            .rev()
            .map(|(k, r)| (k.clone(), r.encode().unwrap()))
            .collect();
        for (k, v) in &enc {
            assert!(decode_mail_row(k, v).is_ok());
        }
        let folded = MailConfig::fold(enc.iter().map(|(k, v)| (k.as_str(), v.as_slice()))).unwrap();
        assert_eq!(folded, cfg);
    }

    /// The fold shows a burned row, hides a revoked one, and keeps the
    /// revoked id spent; the sentinel's owed set is derived from the markers.
    #[test]
    fn the_fold_hides_revoked_shows_burned_and_derives_the_owed_set() {
        let new_msek = SecretArray32::new([3; 32]);
        let mut rows = MailRows {
            state: Some(MailStateRow {
                pending_rotation: Some(MailRotationSentinel {
                    new_msek: new_msek.clone(),
                }),
                ..MailStateRow::default()
            }),
            ..MailRows::default()
        };
        let mut wrapped = cred("wrapped", 1, 1);
        wrapped.wrapped_under = Some(MsekFingerprint::of(&new_msek));
        let mut revoked = cred("revoked", 2, 1);
        revoked.revoked_at_unix = Some(9);
        revoked.secret = Default::default();
        let mut burned = cred("burned", 3, 1);
        burned.burned = Some(MailSuccessionBurn {
            predecessor: ActorId([4; 32]),
            at_unix: 8,
        });
        burned.secret = Default::default();
        let owed = cred("owed", 4, 1);
        for c in [wrapped, revoked, burned, owed] {
            rows.credentials.insert(c.credential_id.clone(), c);
        }
        let cfg = rows.config();
        let ids: Vec<&str> = cfg
            .credentials
            .iter()
            .map(|c| c.credential_id.as_str())
            .collect();
        assert_eq!(ids, ["wrapped", "burned", "owed"]);
        let pending = cfg.pending_rotation.unwrap();
        let owed: Vec<&str> = rows
            .owed_rewrap(&pending.new_msek)
            .into_iter()
            .map(|c| c.credential_id.as_str())
            .collect();
        assert_eq!(owed, ["owed"]);
        assert_eq!(
            rows.spent_credential_ids(),
            ["burned", "owed", "revoked", "wrapped"]
        );
    }

    /// A key outside the grammar, a misfiled credential and a marked
    /// credential holding a secret are refused.
    #[test]
    fn misfiled_unkeyed_or_secret_bearing_marked_rows_are_refused() {
        let c = MailRecord::Credential(cred("a", 1, 1)).encode().unwrap();
        assert!(decode_mail_row("credential/a", &c).is_ok());
        assert!(decode_mail_row("credential/b", &c).is_err());
        assert!(decode_mail_row("credential/", &c).is_err());
        assert!(decode_mail_row("self", &c).is_err());
        assert!(decode_mail_row("mail", &c).is_err());
        let s = MailRecord::State(MailStateRow::default()).encode().unwrap();
        assert!(decode_mail_row("self", &s).is_ok());
        assert!(decode_mail_row("credential/a", &s).is_err());
        let mut marked = cred("a", 1, 1);
        marked.revoked_at_unix = Some(3);
        let v = MailRecord::Credential(marked).encode().unwrap();
        assert!(decode_mail_row("credential/a", &v).is_err());
    }

    /// The heal takes every live row not naming the current generation —
    /// unstamped, a grace generation, one this replica does not know — and
    /// leaves the current rows, the marked rows and, while a rotation is
    /// pending, everything. Its dual takes exactly the marked rows still naming
    /// a generation.
    #[test]
    fn the_heal_takes_every_live_row_off_the_current_generation_and_its_dual_the_marked_rows() {
        let current = SecretArray32::new([1; 32]);
        let prior = SecretArray32::new([2; 32]);
        let unknown = SecretArray32::new([7; 32]);
        let mut rows = MailRows {
            state: Some(MailStateRow {
                msek: Some(current.clone()),
                ..MailStateRow::default()
            }),
            ..MailRows::default()
        };
        rows.join_generation(PriorMsekRetirement {
            msek: prior.clone(),
            retired_at_unix: 1,
        });
        let named = |id: &str, created: u64, gen_: Option<&SecretArray32>| MailCredential {
            wrapped_under: gen_.map(MsekFingerprint::of),
            ..cred(id, created, 1)
        };
        let mut revoked = named("revoked", 5, Some(&prior));
        revoked.revoked_at_unix = Some(9);
        revoked.secret = Default::default();
        let mut burned_clean = named("burned-clean", 6, None);
        burned_clean.burned = Some(MailSuccessionBurn {
            predecessor: ActorId([4; 32]),
            at_unix: 8,
        });
        burned_clean.secret = Default::default();
        for c in [
            named("unstamped", 1, None),
            named("grace", 2, Some(&prior)),
            named("newer", 3, Some(&unknown)),
            named("current", 4, Some(&current)),
            revoked,
            burned_clean,
        ] {
            rows.credentials.insert(c.credential_id.clone(), c);
        }
        let ids = |v: Vec<&MailCredential>| -> Vec<String> {
            v.into_iter().map(|c| c.credential_id.clone()).collect()
        };
        assert_eq!(ids(rows.heal_owed()), ["unstamped", "grace", "newer"]);
        assert_eq!(ids(rows.delete_owed()), ["revoked"]);

        rows.state.as_mut().unwrap().pending_rotation = Some(MailRotationSentinel {
            new_msek: SecretArray32::new([3; 32]),
        });
        assert!(
            rows.heal_owed().is_empty(),
            "a pending rotation owns its own owed set"
        );
    }

    /// A recreatable-half write carries no key material and no sentinel — its
    /// MSEK absent at the zero stamp, its burns empty — so the join keeps the
    /// stored MSEK on the stored key stamp and the stored burns, whatever the
    /// writer's replica held, while the intent's newer flags stamp carries the
    /// flags alone.
    #[test]
    fn a_recreatable_write_never_restates_the_key_material() {
        let stored = MailStateRow {
            updated_at: Timestamp(5),
            ..MailStateRow::from_config(&sample(), Timestamp(5))
        };
        // A replica that never saw the MSEK flips a flag.
        let stale = MailConfig {
            caldav_enabled: false,
            mail_enabled: Some(false),
            ..MailConfig::default()
        };
        let intent = MailStateRow {
            updated_at: Timestamp(6),
            ..MailStateRow::recreatable_of(&stale)
        };
        let joined = stored.merge(&intent);
        assert_eq!(joined.msek, stored.msek, "the MSEK survives");
        assert_eq!(joined.succession_burns, stored.succession_burns);
        assert!(!joined.caldav_enabled, "the newer flags win");
        assert_eq!(joined.mail_enabled, Some(false));
    }

    /// The composite a replica holding only `s` reads.
    fn view(s: &MailStateRow) -> MailConfig {
        MailRows {
            state: Some(s.clone()),
            ..MailRows::default()
        }
        .config()
    }

    /// The door's read-join-put (`fauna_account_plane::mail_rows::write_mail_state`)
    /// at the join: `intent` stamped at `at` over `stored`, joined with it.
    fn door_write(stored: &MailStateRow, intent: &MailStateRow, at: u64) -> MailStateRow {
        stored.merge(&intent.written_over(Some(stored), Timestamp(at)))
    }

    /// A stale replica's flag write, stamped above another device's
    /// committed swap, must not restate its own MSEK at that newer stamp —
    /// the swapped-in key converges on every replica, whichever merges first.
    #[test]
    fn a_stale_flag_write_stamped_above_a_committed_swap_keeps_the_swapped_in_key() {
        let (k1, k2) = (SecretArray32::new([1; 32]), SecretArray32::new([2; 32]));
        let base = MailStateRow {
            msek: Some(k1.clone()),
            mail_enabled: Some(true),
            caldav_enabled: true,
            ..MailStateRow::default()
        };
        let base = door_write(&MailStateRow::default(), &base, 5);
        // Device A swaps in k2 at t7; device B, which has not merged it, flips
        // CalDAV off at t8.
        let a = door_write(
            &base,
            &MailStateRow {
                msek: Some(k2.clone()),
                ..MailStateRow::recreatable_of(&view(&base))
            },
            7,
        );
        assert_eq!(a.msek.as_ref(), Some(&k2), "A's swap committed on A");
        let b_view = MailConfig {
            caldav_enabled: false,
            ..view(&base)
        };
        let b = door_write(&base, &MailStateRow::recreatable_of(&b_view), 8);
        assert_eq!(a.merge(&b), b.merge(&a), "the join commutes");
        let converged = a.merge(&b);
        assert_eq!(converged.msek.as_ref(), Some(&k2), "the swap survives");
        assert!(!converged.caldav_enabled, "B's newer flag still wins");
    }

    /// The sentinel twin: a stale replica that never saw a rotation's
    /// sentinel flips a flag above it — the sentinel survives the join.
    #[test]
    fn a_stale_flag_write_never_clears_a_sentinel_its_replica_never_saw() {
        let (k1, k2) = (SecretArray32::new([1; 32]), SecretArray32::new([2; 32]));
        let base = door_write(
            &MailStateRow::default(),
            &MailStateRow {
                msek: Some(k1),
                mail_enabled: Some(true),
                caldav_enabled: true,
                ..MailStateRow::default()
            },
            5,
        );
        let a = door_write(
            &base,
            &MailStateRow {
                pending_rotation: Some(MailRotationSentinel {
                    new_msek: k2.clone(),
                }),
                ..MailStateRow::recreatable_of(&view(&base))
            },
            7,
        );
        let b_view = MailConfig {
            caldav_enabled: false,
            ..view(&base)
        };
        let b = door_write(&base, &MailStateRow::recreatable_of(&b_view), 8);
        assert_eq!(a.merge(&b), b.merge(&a), "the join commutes");
        let converged = a.merge(&b);
        assert_eq!(
            converged.pending_rotation.map(|s| s.new_msek),
            Some(k2),
            "the sentinel survives"
        );
        assert!(!converged.caldav_enabled, "B's newer flag still wins");
    }

    /// The sentinel's clear names the MSEK′ it clears: a clear naming another,
    /// or over no sentinel, is no clear; the named clear wins the join over
    /// the sentinel it saw.
    #[test]
    fn a_rotation_clear_must_name_the_sentinel_it_clears() {
        let (k1, k2) = (SecretArray32::new([1; 32]), SecretArray32::new([2; 32]));
        let pending = door_write(
            &MailStateRow::default(),
            &MailStateRow {
                msek: Some(k1.clone()),
                pending_rotation: Some(MailRotationSentinel {
                    new_msek: k2.clone(),
                }),
                ..MailStateRow::default()
            },
            5,
        );
        assert!(MailStateRow::rotation_cleared(&pending, &k1, Timestamp(6)).is_none());
        assert!(
            MailStateRow::rotation_cleared(&MailStateRow::default(), &k2, Timestamp(6)).is_none()
        );
        let cleared = MailStateRow::rotation_cleared(&pending, &k2, Timestamp(6)).unwrap();
        assert_eq!(cleared.pending_rotation, None);
        assert_eq!(
            cleared.msek.as_ref(),
            Some(&k1),
            "the clear moves nothing else"
        );
        assert_eq!(pending.merge(&cleared), cleared.merge(&pending));
        assert_eq!(pending.merge(&cleared).pending_rotation, None);
        // A restating write from the replica that still holds the sentinel
        // cannot resurrect it over the clear.
        let restated = door_write(&pending, &MailStateRow::recreatable_of(&view(&pending)), 7);
        assert_eq!(restated.merge(&cleared).pending_rotation, None);
    }

    fn generation(seed: u8, at: u64) -> PriorMsekRetirement {
        PriorMsekRetirement {
            msek: SecretArray32::new([seed; 32]),
            retired_at_unix: at,
        }
    }

    /// **Every generation ever retired is carried, uncapped** (`owner-key-material.md`
    /// § Path B-sibling-2 → *Pre-rotation mail at rest*): five generation rows
    /// fold into five priors, most recently retired first, each with its own
    /// instant, the current MSEK excluded — whatever order the rows arrive in.
    #[test]
    fn the_fold_carries_every_generation_most_recent_first() {
        let state = MailRecord::State(MailStateRow {
            msek: Some(SecretArray32::new([9; 32])),
            ..MailStateRow::default()
        });
        let mut recs = vec![state];
        // Generation `n` retired at `100 * n`; plus a stray row for the
        // current MSEK, which the fold must not list as a prior.
        for n in [3u8, 1, 5, 2, 4] {
            recs.push(MailRecord::Generation(generation(n, 100 * u64::from(n))));
        }
        recs.push(MailRecord::Generation(generation(9, 600)));
        let enc: Vec<(String, Vec<u8>)> = recs
            .iter()
            .map(|r| (r.plane_key().unwrap(), r.encode().unwrap()))
            .collect();
        let cfg = MailConfig::fold(enc.iter().map(|(k, v)| (k.as_str(), v.as_slice()))).unwrap();
        let seeds: Vec<u8> = cfg.prior_mseks.iter().map(|k| k.as_ref()[0]).collect();
        assert_eq!(
            seeds,
            [5, 4, 3, 2, 1],
            "uncapped, most recently retired first"
        );
        let instants: Vec<u64> = cfg
            .prior_msek_retirements
            .iter()
            .map(|r| r.retired_at_unix)
            .collect();
        assert_eq!(instants, [500, 400, 300, 200, 100]);
        // And the composite splits back into the same five rows.
        let regen = cfg.generation_rows().unwrap();
        assert_eq!(regen.len(), 5);
        assert!(
            regen
                .iter()
                .zip(&cfg.prior_msek_retirements)
                .all(|(a, b)| a == b)
        );
    }

    /// A generation row is keyed by its fingerprint in lowercase hex; a row
    /// filed under another generation's key, an uppercase or short key, and a
    /// value carrying an unknown field are refused. Two records of one
    /// retirement join on the later instant, in either order.
    #[test]
    fn a_generation_row_is_keyed_by_its_fingerprint_and_joins_on_the_later_instant() {
        let g = MailRecord::Generation(generation(4, 70));
        let key = g.plane_key().unwrap();
        assert_eq!(
            key,
            format!(
                "generation/{}",
                hex::encode(MsekFingerprint::of(&SecretArray32::new([4; 32])).0)
            )
        );
        let v = g.encode().unwrap();
        assert!(matches!(
            decode_mail_row(&key, &v),
            Ok(MailRecord::Generation(_))
        ));
        let other_key = MailRecord::Generation(generation(5, 70))
            .plane_key()
            .unwrap();
        assert!(decode_mail_row(&other_key, &v).is_err(), "misfiled");
        assert!(
            decode_mail_row(&key.to_uppercase().replace("GENERATION", "generation"), &v).is_err()
        );
        assert!(decode_mail_row(&key[..key.len() - 2], &v).is_err());
        assert!(decode_mail_row("generation/", &v).is_err());
        assert!(decode_mail_row("self", &v).is_err());

        let early = MailRecord::Generation(generation(4, 70));
        let late = MailRecord::Generation(generation(4, 90));
        for (a, b) in [(&early, &late), (&late, &early)] {
            let MailRecord::Generation(j) = a.merge(b).unwrap() else {
                panic!("a generation joins as a generation");
            };
            assert_eq!(j.retired_at_unix, 90);
        }
        assert!(
            early
                .merge(&MailRecord::Generation(generation(5, 70)))
                .is_err(),
            "two generations are different rows"
        );
    }

    /// The fingerprint is the frozen derivation, and differs per generation.
    #[test]
    fn the_msek_fingerprint_is_the_frozen_derivation() {
        let k = SecretArray32::new([5; 32]);
        assert_eq!(
            MsekFingerprint::of(&k).0,
            blake3::derive_key("fauna.mail.msek-fingerprint.v1", &[5; 32])
        );
        assert_ne!(
            MsekFingerprint::of(&k),
            MsekFingerprint::of(&SecretArray32::new([6; 32]))
        );
    }

    /// The token-mint stamp joins on the later mint, never on the row stamp:
    /// a rename that read the row before a re-mint, stamped after it, keeps
    /// its name but cannot set the refresh back (`mail-credentials.md` rule 10).
    #[test]
    fn the_token_mint_stamp_joins_on_the_later_mint_whatever_the_row_stamp() {
        let mut reminted = cred("a-mail", 10, 5);
        reminted.token_minted_at = Some(1_000);
        let mut renamed = cred("a-mail", 10, 9);
        renamed.display_name = "renamed".into();
        renamed.token_minted_at = Some(400);
        for joined in [reminted.merge(&renamed), renamed.merge(&reminted)] {
            assert_eq!(joined.display_name, "renamed");
            assert_eq!(joined.token_minted_at, Some(1_000));
        }
        assert_eq!(
            cred("a-mail", 10, 9).merge(&reminted).token_minted_at,
            Some(1_000)
        );
        let encoded = MailRecord::Credential(reminted.clone()).encode().unwrap();
        let MailRecord::Credential(back) = decode_mail_row("credential/a-mail", &encoded).unwrap()
        else {
            panic!("a credential row decodes as one");
        };
        assert_eq!(back, reminted);
    }
}
